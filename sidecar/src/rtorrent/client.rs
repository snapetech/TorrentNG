use anyhow::{anyhow, bail, Context, Result};
use bytes::{BufMut, BytesMut};
use quick_xml::{events::Event, name::QName, Reader};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;

use crate::config::RtorrentConfig;

const MAX_SCGI_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
// XML-RPC responses can be large for a raw metainfo load, but their decoded
// shape must stay bounded independently of the wire size.  These limits also
// protect the JSON-RPC compatibility path, which is converted into XmlValue
// before callers inspect it.
const MAX_XMLRPC_REQUEST_BYTES: usize = 128 * 1024 * 1024;
const MAX_XMLRPC_COLLECTION_ITEMS: usize = 16_384;
const MAX_XMLRPC_VALUE_DEPTH: usize = 64;
const MAX_XMLRPC_TEXT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub enum Transport {
    Unix(String),
    Tcp(String),
}

#[derive(Clone)]
pub struct Client {
    transport: Transport,
    timeout: std::time::Duration,
    identity_timeout: std::time::Duration,
    rpc_gate: Arc<Semaphore>,
    low_priority_pause_until: Arc<Mutex<Option<Instant>>>,
    tracker_peer_id: Option<String>,
}

impl Client {
    pub fn new(cfg: &RtorrentConfig) -> Result<Self> {
        let transport = match (&cfg.scgi_socket, &cfg.scgi_addr) {
            (Some(path), None) => Transport::Unix(path.clone()),
            (None, Some(addr)) => Transport::Tcp(addr.clone()),
            _ => bail!("exactly one of scgi_socket or scgi_addr must be set"),
        };
        Ok(Self {
            transport,
            timeout: std::time::Duration::from_secs(cfg.timeout_secs),
            identity_timeout: std::time::Duration::from_secs(cfg.identity_timeout_secs),
            rpc_gate: Arc::new(Semaphore::new(1)),
            low_priority_pause_until: Arc::new(Mutex::new(None)),
            tracker_peer_id: Some(cfg.peer_id.clone()),
        })
    }

    /// Construct a Unix-socket client without a full config — for tests.
    pub fn new_unix(socket_path: &str, timeout_secs: u64) -> Self {
        Self {
            transport: Transport::Unix(socket_path.to_owned()),
            timeout: std::time::Duration::from_secs(timeout_secs),
            identity_timeout: std::time::Duration::from_secs(timeout_secs),
            rpc_gate: Arc::new(Semaphore::new(1)),
            low_priority_pause_until: Arc::new(Mutex::new(None)),
            tracker_peer_id: None,
        }
    }

    /// Return the resolved per-install tracker identity for commands that
    /// create or resume a download. Test-only clients intentionally have no
    /// identity because they do not own a configured rTorrent instance.
    pub(crate) fn tracker_peer_id(&self) -> Option<&str> {
        self.tracker_peer_id.as_deref()
    }

    /// Execute a single XMLRPC method and return the parsed result.
    pub async fn call(&self, method: &str, args: &[XmlValue]) -> Result<XmlValue> {
        self.call_with_priority(method, args, RpcPriority::User)
            .await
    }

    /// Execute a single XML-RPC method without trying rTorrent's JSON-RPC
    /// adapter first. Some rTorrent mutators return success through JSON-RPC
    /// without applying the state transition.
    pub async fn call_xmlrpc(&self, method: &str, args: &[XmlValue]) -> Result<XmlValue> {
        let _permit = self
            .rpc_gate
            .acquire()
            .await
            .context("rTorrent RPC gate closed")?;
        self.call_xml(method, args).await
    }

    /// Execute a startup/control XML-RPC call with an operation-specific
    /// timeout. Large session rewrites must not force ordinary list, stats, or
    /// user-control calls to wait for the same multi-minute budget.
    pub(crate) async fn call_identity_xmlrpc(
        &self,
        method: &str,
        args: &[XmlValue],
    ) -> Result<XmlValue> {
        let _permit = self
            .rpc_gate
            .acquire()
            .await
            .context("rTorrent RPC gate closed")?;
        self.call_xml_with_timeout(method, args, self.identity_timeout)
            .await
    }

    pub async fn call_sync(&self, method: &str, args: &[XmlValue]) -> Result<XmlValue> {
        self.call_with_priority(method, args, RpcPriority::Background)
            .await
    }

    /// Execute many independent, single-argument XMLRPC calls in one round
    /// trip via the standard `system.multicall` meta-method, instead of one
    /// connection + request/response per call. rTorrent processes them in
    /// array order within a single request; each entry reports its own
    /// success/fault independently of the others. Confirmed supported by
    /// this project's patched rTorrent 0.16.11 build (see docs/TRACKER-IDENTITY.md
    /// for the other rTorrent patches this codebase carries).
    ///
    /// `calls` is `(method, single_arg)` pairs, e.g. `("d.stop", hash)`.
    /// Returns one `Result` per input call, in the same order.
    pub async fn call_multicall(
        &self,
        method: &str,
        args: &[String],
    ) -> Result<Vec<Result<XmlValue, String>>> {
        if args.is_empty() {
            return Ok(Vec::new());
        }
        let entries: Vec<XmlValue> = args
            .iter()
            .map(|arg| {
                XmlValue::Struct(vec![
                    ("methodName".to_owned(), XmlValue::String(method.to_owned())),
                    (
                        "params".to_owned(),
                        XmlValue::Array(vec![XmlValue::String(arg.clone())]),
                    ),
                ])
            })
            .collect();

        let _permit = self
            .rpc_gate
            .acquire()
            .await
            .context("rTorrent RPC gate closed")?;
        let response = self
            .call_xml("system.multicall", &[XmlValue::Array(entries)])
            .await
            .with_context(|| format!("system.multicall({method})"))?;

        parse_multicall_response(response, args.len(), method)
    }

    async fn call_with_priority(
        &self,
        method: &str,
        args: &[XmlValue],
        priority: RpcPriority,
    ) -> Result<XmlValue> {
        if priority == RpcPriority::Background {
            let mut pause = self.low_priority_pause_until.lock().await;
            if let Some(until) = *pause {
                if until > Instant::now() {
                    bail!("rTorrent RPC circuit breaker is open");
                }
                *pause = None;
            }
        }

        let _permit = self
            .rpc_gate
            .acquire()
            .await
            .context("rTorrent RPC gate closed")?;

        let result = match self.call_json(method, args).await {
            Ok(value) => Ok(value),
            Err(json_err) => {
                if is_jsonrpc_unavailable(&json_err) {
                    self.call_xml(method, args).await
                } else {
                    Err(json_err)
                }
            }
        };
        if priority == RpcPriority::Background {
            if let Err(error) = &result {
                if is_timeout_error(error) {
                    *self.low_priority_pause_until.lock().await =
                        Some(Instant::now() + std::time::Duration::from_secs(15));
                }
            }
        }
        result
    }

    async fn call_json(&self, method: &str, args: &[XmlValue]) -> Result<XmlValue> {
        if args.iter().any(contains_base64) {
            bail!("JSON-RPC unavailable for XML-RPC base64 payload");
        }
        if method.len() > MAX_XMLRPC_TEXT_BYTES {
            bail!("JSON-RPC method name exceeds {MAX_XMLRPC_TEXT_BYTES} byte limit");
        }
        if args.len() > MAX_XMLRPC_COLLECTION_ITEMS {
            bail!("JSON-RPC parameter count exceeds {MAX_XMLRPC_COLLECTION_ITEMS} items");
        }
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": args.iter().map(xml_to_json).collect::<Vec<_>>(),
        })
        .to_string();
        if body.len() > MAX_XMLRPC_REQUEST_BYTES {
            bail!("JSON-RPC request exceeds {MAX_XMLRPC_REQUEST_BYTES} byte limit");
        }
        let response = self
            .scgi_roundtrip("application/json", body.as_bytes())
            .await
            .with_context(|| format!("JSON-RPC call {method}"))?;
        parse_jsonrpc_response(&response)
    }

    async fn call_xml(&self, method: &str, args: &[XmlValue]) -> Result<XmlValue> {
        self.call_xml_with_timeout(method, args, self.timeout).await
    }

    async fn call_xml_with_timeout(
        &self,
        method: &str,
        args: &[XmlValue],
        timeout: std::time::Duration,
    ) -> Result<XmlValue> {
        let body = build_xmlrpc_request(method, args)?;
        let response = self
            .scgi_roundtrip_with_timeout("text/xml", body.as_bytes(), timeout)
            .await
            .with_context(|| format!("XMLRPC call {method}"))?;
        parse_xmlrpc_response(&response)
    }

    /// Send raw SCGI request and return the HTTP body.
    async fn scgi_roundtrip(&self, content_type: &str, body: &[u8]) -> Result<Vec<u8>> {
        self.scgi_roundtrip_with_timeout(content_type, body, self.timeout)
            .await
    }

    async fn scgi_roundtrip_with_timeout(
        &self,
        content_type: &str,
        body: &[u8],
        timeout: std::time::Duration,
    ) -> Result<Vec<u8>> {
        let content_length = body.len();
        let headers = format!(
            "CONTENT_LENGTH\0{content_length}\0SCGI\01\0REQUEST_METHOD\0POST\0\
             REQUEST_URI\0/RPC2\0CONTENT_TYPE\0{content_type}\0"
        );
        let netstring = format!("{}:{},", headers.len(), headers);

        let mut packet = BytesMut::with_capacity(netstring.len() + body.len());
        packet.put(netstring.as_bytes());
        packet.put(body);

        let response = tokio::time::timeout(timeout, async {
            match &self.transport {
                Transport::Unix(path) => {
                    #[cfg(not(unix))]
                    bail!("Unix SCGI sockets are unsupported on this platform: {path}");

                    #[cfg(unix)]
                    {
                        let mut stream = UnixStream::connect(path)
                            .await
                            .with_context(|| format!("connect to SCGI socket {path}"))?;
                        stream.write_all(&packet).await?;
                        let mut buf = Vec::new();
                        let mut limited = stream.take((MAX_SCGI_RESPONSE_BYTES + 1) as u64);
                        limited.read_to_end(&mut buf).await?;
                        if buf.len() > MAX_SCGI_RESPONSE_BYTES {
                            bail!("SCGI response exceeds {MAX_SCGI_RESPONSE_BYTES} byte limit");
                        }
                        Ok::<_, anyhow::Error>(buf)
                    }
                }
                Transport::Tcp(addr) => {
                    let mut stream = TcpStream::connect(addr)
                        .await
                        .with_context(|| format!("connect to SCGI addr {addr}"))?;
                    stream.write_all(&packet).await?;
                    let mut buf = Vec::new();
                    let mut limited = stream.take((MAX_SCGI_RESPONSE_BYTES + 1) as u64);
                    limited.read_to_end(&mut buf).await?;
                    if buf.len() > MAX_SCGI_RESPONSE_BYTES {
                        bail!("SCGI response exceeds {MAX_SCGI_RESPONSE_BYTES} byte limit");
                    }
                    Ok(buf)
                }
            }
        })
        .await
        .context("SCGI call timed out")??;

        // Strip HTTP headers (everything up to \r\n\r\n)
        let body_start = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| i + 4)
            .unwrap_or(0);

        Ok(response[body_start..].to_vec())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RpcPriority {
    User,
    Background,
}

// --- XMLRPC types ---

#[derive(Debug, Clone)]
pub enum XmlValue {
    String(String),
    Base64(String),
    Int(i64),
    Bool(bool),
    Array(Vec<XmlValue>),
    Struct(Vec<(String, XmlValue)>),
    Nil,
}

impl XmlValue {
    pub fn as_str(&self) -> Option<&str> {
        if let XmlValue::String(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            XmlValue::Int(n) => Some(*n),
            XmlValue::String(s) => s.parse().ok(),
            XmlValue::Base64(s) => s.parse().ok(),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            XmlValue::Bool(b) => Some(*b),
            XmlValue::Int(n) => Some(*n != 0),
            _ => None,
        }
    }
    pub fn into_array(self) -> Vec<XmlValue> {
        if let XmlValue::Array(v) = self {
            v
        } else {
            vec![]
        }
    }

    pub fn try_into_array(self) -> Result<Vec<XmlValue>> {
        match self {
            XmlValue::Array(values) => Ok(values),
            other => bail!("expected XML-RPC array, got {}", describe_xml_value(&other)),
        }
    }
}

impl From<&str> for XmlValue {
    fn from(s: &str) -> Self {
        XmlValue::String(s.to_owned())
    }
}
impl From<String> for XmlValue {
    fn from(s: String) -> Self {
        XmlValue::String(s)
    }
}
impl From<i64> for XmlValue {
    fn from(n: i64) -> Self {
        XmlValue::Int(n)
    }
}
impl From<bool> for XmlValue {
    fn from(b: bool) -> Self {
        XmlValue::Bool(b)
    }
}

fn xml_to_json(value: &XmlValue) -> Value {
    match value {
        XmlValue::String(s) | XmlValue::Base64(s) => Value::String(s.clone()),
        XmlValue::Int(n) => json!(n),
        XmlValue::Bool(b) => json!(b),
        XmlValue::Array(items) => Value::Array(items.iter().map(xml_to_json).collect()),
        XmlValue::Struct(fields) => Value::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), xml_to_json(v)))
                .collect(),
        ),
        XmlValue::Nil => Value::Null,
    }
}

fn json_to_xml(value: Value) -> Result<XmlValue> {
    json_to_xml_at_depth(value, 0)
}

fn json_to_xml_at_depth(value: Value, depth: usize) -> Result<XmlValue> {
    if depth > MAX_XMLRPC_VALUE_DEPTH {
        bail!("JSON-RPC value nesting exceeds {MAX_XMLRPC_VALUE_DEPTH} levels");
    }
    Ok(match value {
        Value::Null => XmlValue::Nil,
        Value::Bool(b) => XmlValue::Bool(b),
        Value::Number(n) => XmlValue::Int(
            n.as_i64()
                .or_else(|| n.as_u64().and_then(|value| i64::try_from(value).ok()))
                .ok_or_else(|| anyhow!("JSON-RPC number does not fit in signed 64 bits"))?,
        ),
        Value::String(s) => {
            if s.len() > MAX_XMLRPC_TEXT_BYTES {
                bail!("JSON-RPC text exceeds {MAX_XMLRPC_TEXT_BYTES} byte limit");
            }
            XmlValue::String(s)
        }
        Value::Array(items) => {
            if items.len() > MAX_XMLRPC_COLLECTION_ITEMS {
                bail!("JSON-RPC array exceeds {MAX_XMLRPC_COLLECTION_ITEMS} items");
            }
            XmlValue::Array(
                items
                    .into_iter()
                    .map(|value| json_to_xml_at_depth(value, depth + 1))
                    .collect::<Result<Vec<_>>>()?,
            )
        }
        Value::Object(map) => {
            if map.len() > MAX_XMLRPC_COLLECTION_ITEMS {
                bail!("JSON-RPC object exceeds {MAX_XMLRPC_COLLECTION_ITEMS} members");
            }
            XmlValue::Struct(
                map.into_iter()
                    .map(|(key, value)| {
                        if key.len() > MAX_XMLRPC_TEXT_BYTES {
                            bail!("JSON-RPC object key exceeds {MAX_XMLRPC_TEXT_BYTES} byte limit");
                        }
                        Ok((key, json_to_xml_at_depth(value, depth + 1)?))
                    })
                    .collect::<Result<Vec<_>>>()?,
            )
        }
    })
}

fn parse_jsonrpc_response(body: &[u8]) -> Result<XmlValue> {
    let value: Value =
        serde_json::from_slice(body).context("JSON-RPC response is not valid JSON")?;
    let response = value
        .as_object()
        .ok_or_else(|| anyhow!("JSON-RPC response is not an object"))?;
    if let Some(error) = response.get("error").filter(|error| !error.is_null()) {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        if message.len() > MAX_XMLRPC_TEXT_BYTES {
            bail!("JSON-RPC error message exceeds {MAX_XMLRPC_TEXT_BYTES} byte limit");
        }
        bail!("JSON-RPC error: {}", message);
    }
    let result = response
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("JSON-RPC response has no result"))?;
    json_to_xml(result)
}

fn is_jsonrpc_unavailable(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}");
    text.contains("JSON-RPC not supported")
        || text.contains("JSON-RPC unavailable for XML-RPC base64 payload")
        || text.contains("method not found: system.listMethods")
        || text.contains("method not found: method.list_keys")
}

fn contains_base64(value: &XmlValue) -> bool {
    match value {
        XmlValue::Base64(_) => true,
        XmlValue::Array(items) => items.iter().any(contains_base64),
        XmlValue::Struct(fields) => fields.iter().any(|(_, value)| contains_base64(value)),
        _ => false,
    }
}

fn is_timeout_error(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}");
    text.contains("timed out") || text.contains("deadline has elapsed")
}

// --- XMLRPC builder ---

fn build_xmlrpc_request(method: &str, args: &[XmlValue]) -> Result<String> {
    if method.len() > MAX_XMLRPC_TEXT_BYTES {
        bail!("XML-RPC method name exceeds {MAX_XMLRPC_TEXT_BYTES} byte limit");
    }
    if args.len() > MAX_XMLRPC_COLLECTION_ITEMS {
        bail!("XML-RPC parameter count exceeds {MAX_XMLRPC_COLLECTION_ITEMS} items");
    }
    let mut out = String::from("<?xml version=\"1.0\"?>\n<methodCall>\n");
    out.push_str(&format!(
        "  <methodName>{}</methodName>\n  <params>\n",
        xml_escape(method)
    ));
    for arg in args {
        out.push_str("    <param><value>");
        write_xml_value(&mut out, arg, 0)?;
        out.push_str("</value></param>\n");
    }
    out.push_str("  </params>\n</methodCall>");
    if out.len() > MAX_XMLRPC_REQUEST_BYTES {
        bail!("XML-RPC request exceeds {MAX_XMLRPC_REQUEST_BYTES} byte limit");
    }
    Ok(out)
}

fn write_xml_value(out: &mut String, v: &XmlValue, depth: usize) -> Result<()> {
    if depth > MAX_XMLRPC_VALUE_DEPTH {
        bail!("XML-RPC value nesting exceeds {MAX_XMLRPC_VALUE_DEPTH} levels");
    }
    match v {
        XmlValue::String(s) => {
            if s.len() > MAX_XMLRPC_TEXT_BYTES {
                bail!("XML-RPC text exceeds {MAX_XMLRPC_TEXT_BYTES} byte limit");
            }
            out.push_str("<string>");
            out.push_str(&xml_escape(s));
            out.push_str("</string>");
        }
        XmlValue::Base64(s) => {
            if s.len() > MAX_XMLRPC_REQUEST_BYTES {
                bail!("XML-RPC base64 value exceeds {MAX_XMLRPC_REQUEST_BYTES} bytes");
            }
            out.push_str("<base64>");
            out.push_str(s);
            out.push_str("</base64>");
        }
        XmlValue::Int(n) => {
            out.push_str(&format!("<i8>{n}</i8>"));
        }
        XmlValue::Bool(b) => {
            out.push_str(&format!("<boolean>{}</boolean>", if *b { 1 } else { 0 }));
        }
        XmlValue::Array(items) => {
            if items.len() > MAX_XMLRPC_COLLECTION_ITEMS {
                bail!("XML-RPC array exceeds {MAX_XMLRPC_COLLECTION_ITEMS} items");
            }
            out.push_str("<array><data>");
            for item in items {
                out.push_str("<value>");
                write_xml_value(out, item, depth + 1)?;
                out.push_str("</value>");
            }
            out.push_str("</data></array>");
        }
        XmlValue::Struct(fields) => {
            if fields.len() > MAX_XMLRPC_COLLECTION_ITEMS {
                bail!("XML-RPC struct exceeds {MAX_XMLRPC_COLLECTION_ITEMS} members");
            }
            out.push_str("<struct>");
            for (k, v) in fields {
                if k.len() > MAX_XMLRPC_TEXT_BYTES {
                    bail!("XML-RPC struct key exceeds {MAX_XMLRPC_TEXT_BYTES} byte limit");
                }
                out.push_str(&format!("<member><name>{}</name><value>", xml_escape(k)));
                write_xml_value(out, v, depth + 1)?;
                out.push_str("</value></member>");
            }
            out.push_str("</struct>");
        }
        XmlValue::Nil => {
            out.push_str("<nil/>");
        }
    }
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn describe_xml_value(value: &XmlValue) -> String {
    match value {
        XmlValue::String(value) | XmlValue::Base64(value) => value.clone(),
        XmlValue::Int(value) => value.to_string(),
        XmlValue::Bool(value) => value.to_string(),
        XmlValue::Nil => "nil".to_owned(),
        XmlValue::Array(items) => items
            .iter()
            .map(describe_xml_value)
            .collect::<Vec<_>>()
            .join(", "),
        XmlValue::Struct(fields) => fields
            .iter()
            .map(|(key, value)| format!("{key}={}", describe_xml_value(value)))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

// --- XMLRPC parser ---

/// Turn a parsed `system.multicall` response array into one Result per
/// input call. Per the XMLRPC multicall convention: a successful entry is
/// a one-element array wrapping the real return value; a failed entry is a
/// `{faultCode, faultString}` struct. Confirmed against this project's
/// patched rTorrent 0.16.11 build via a raw SCGI probe (both shapes).
fn parse_multicall_response(
    response: XmlValue,
    expected_count: usize,
    method: &str,
) -> Result<Vec<Result<XmlValue, String>>> {
    let items = response.try_into_array()?;
    if items.len() != expected_count {
        bail!(
            "system.multicall({method}) returned {} results for {} calls",
            items.len(),
            expected_count
        );
    }
    Ok(items
        .into_iter()
        .map(|item| match item {
            // Per-call fault: {faultCode, faultString}.
            XmlValue::Struct(fields) => Err(fields
                .into_iter()
                .find(|(k, _)| k == "faultString")
                .and_then(|(_, v)| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown multicall fault".to_owned())),
            // Per-call success: single-element array wrapping the value.
            XmlValue::Array(mut inner) if inner.len() == 1 => Ok(inner.pop().unwrap()),
            XmlValue::Array(_) => {
                Err("multicall success entry did not contain one value".to_owned())
            }
            other => Ok(other),
        })
        .collect())
}

fn parse_xmlrpc_response(xml: &[u8]) -> Result<XmlValue> {
    let xml_str = std::str::from_utf8(xml).context("XMLRPC response not UTF-8")?;
    let mut reader = Reader::from_str(xml_str);
    reader.config_mut().trim_text(true);

    // Seek to <methodResponse>
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.name().into_inner() == "methodResponse" => break,
            Event::Eof => bail!("XMLRPC response missing <methodResponse>"),
            _ => {}
        }
    }

    // Check for <fault> vs <params>
    loop {
        match reader.read_event()? {
            Event::Start(e) => match e.name().into_inner() {
                "fault" => {
                    let fault = parse_value(&mut reader)?;
                    bail!(
                        "XMLRPC fault returned by rTorrent: {}",
                        describe_xml_value(&fault)
                    )
                }
                "params" => break,
                _ => {}
            },
            Event::Eof => bail!("unexpected EOF in XMLRPC response"),
            _ => {}
        }
    }

    parse_value(&mut reader)
}

fn parse_value(reader: &mut Reader<&[u8]>) -> Result<XmlValue> {
    // Seek to <value>
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.name().into_inner() == "value" => break,
            Event::End(_) => return Err(anyhow!("XML-RPC response missing <value>")),
            Event::Eof => return Err(anyhow!("XML-RPC response ended before <value>")),
            _ => {}
        }
    }
    parse_value_content(reader, 0)
}

fn parse_value_content(reader: &mut Reader<&[u8]>, depth: usize) -> Result<XmlValue> {
    if depth > MAX_XMLRPC_VALUE_DEPTH {
        bail!("XML-RPC value nesting exceeds {MAX_XMLRPC_VALUE_DEPTH} levels");
    }
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let val = match e.name().into_inner() {
                    "string" => {
                        let text = read_text_string(reader, e.name())?;
                        XmlValue::String(text)
                    }
                    "int" | "i4" | "i8" => {
                        let text = read_text_string(reader, e.name())?;
                        XmlValue::Int(text.trim().parse().with_context(|| {
                            format!("invalid XML-RPC integer value {:?}", text.trim())
                        })?)
                    }
                    "boolean" => {
                        let text = read_text_string(reader, e.name())?;
                        XmlValue::Bool(match text.trim() {
                            "0" => false,
                            "1" => true,
                            value => bail!("invalid XML-RPC boolean value {value:?}"),
                        })
                    }
                    "array" => parse_array(reader, depth + 1)?,
                    "struct" => parse_struct(reader, depth + 1)?,
                    "nil" => {
                        reader.read_text(e.name())?;
                        XmlValue::Nil
                    }
                    _ => {
                        let text = read_text_string(reader, e.name())?;
                        XmlValue::String(text.trim().to_owned())
                    }
                };
                return Ok(val);
            }
            Event::Text(t) => {
                let text = t.into_inner();
                if text.len() > MAX_XMLRPC_TEXT_BYTES {
                    bail!("XML-RPC text exceeds {MAX_XMLRPC_TEXT_BYTES} byte limit");
                }
                let s = text.trim().to_owned();
                if !s.is_empty() {
                    return Ok(XmlValue::String(s));
                }
            }
            Event::End(_) => return Ok(XmlValue::Nil),
            Event::Eof => return Err(anyhow!("unexpected EOF in value")),
            _ => {}
        }
    }
}

fn parse_array(reader: &mut Reader<&[u8]>, depth: usize) -> Result<XmlValue> {
    if depth > MAX_XMLRPC_VALUE_DEPTH {
        bail!("XML-RPC value nesting exceeds {MAX_XMLRPC_VALUE_DEPTH} levels");
    }
    let mut items = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.name().into_inner() == "value" => {
                if items.len() >= MAX_XMLRPC_COLLECTION_ITEMS {
                    bail!("XML-RPC array exceeds {MAX_XMLRPC_COLLECTION_ITEMS} items");
                }
                items.push(parse_value_content(reader, depth)?);
            }
            Event::End(e) if e.name().into_inner() == "array" => break,
            Event::Eof => return Err(anyhow!("unexpected EOF in XML-RPC array")),
            _ => {}
        }
    }
    Ok(XmlValue::Array(items))
}

fn parse_struct(reader: &mut Reader<&[u8]>, depth: usize) -> Result<XmlValue> {
    if depth > MAX_XMLRPC_VALUE_DEPTH {
        bail!("XML-RPC value nesting exceeds {MAX_XMLRPC_VALUE_DEPTH} levels");
    }
    let mut fields = Vec::new();
    let mut current_name = String::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) => match e.name().into_inner() {
                "name" => {
                    current_name = read_text_string(reader, e.name())?;
                }
                "value" => {
                    if fields.len() >= MAX_XMLRPC_COLLECTION_ITEMS {
                        bail!("XML-RPC struct exceeds {MAX_XMLRPC_COLLECTION_ITEMS} members");
                    }
                    let val = parse_value_content(reader, depth)?;
                    fields.push((std::mem::take(&mut current_name), val));
                }
                _ => {}
            },
            Event::End(e) if e.name().into_inner() == "struct" => break,
            Event::Eof => return Err(anyhow!("unexpected EOF in XML-RPC struct")),
            _ => {}
        }
    }
    Ok(XmlValue::Struct(fields))
}

fn read_text_string(reader: &mut Reader<&[u8]>, end: QName<'_>) -> Result<String> {
    let text = reader.read_text(end)?.into_inner().into_owned();
    if text.len() > MAX_XMLRPC_TEXT_BYTES {
        bail!("XML-RPC text exceeds {MAX_XMLRPC_TEXT_BYTES} byte limit");
    }
    Ok(text)
}

#[cfg(test)]
mod multicall_tests {
    use super::*;

    // Exact shapes observed from a real raw SCGI probe against this
    // project's patched rTorrent 0.16.11 build:
    //   success: <value><array><data><value><i8>0</i8></value></data></array></value>
    //   fault:   <value><struct><member><name>faultCode</name>...
    //                    <member><name>faultString</name>
    //                       <value><string>invalid parameters: invalid target</string>...

    #[test]
    fn all_success_unwraps_each_single_element_array() {
        let response = XmlValue::Array(vec![
            XmlValue::Array(vec![XmlValue::Int(0)]),
            XmlValue::Array(vec![XmlValue::Int(0)]),
        ]);
        let results = parse_multicall_response(response, 2, "d.stop").unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok());
        assert!(results[1].is_ok());
        assert_eq!(results[0].as_ref().unwrap().as_i64(), Some(0));
    }

    #[test]
    fn per_call_fault_reports_that_hash_without_failing_the_others() {
        let fault = XmlValue::Struct(vec![
            ("faultCode".to_owned(), XmlValue::Int(-500)),
            (
                "faultString".to_owned(),
                XmlValue::String("invalid parameters: invalid target".to_owned()),
            ),
        ]);
        let response = XmlValue::Array(vec![fault, XmlValue::Array(vec![XmlValue::Int(0)])]);
        let results = parse_multicall_response(response, 2, "d.stop").unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].as_ref().unwrap_err(),
            "invalid parameters: invalid target"
        );
        assert!(
            results[1].is_ok(),
            "one fault must not affect the other result"
        );
    }

    #[test]
    fn result_count_mismatch_is_an_error_not_a_panic() {
        // Guards the zip() in stop_many()/recheck_many() against ever being
        // fed a shorter results list than the hashes it's paired with.
        let response = XmlValue::Array(vec![XmlValue::Array(vec![XmlValue::Int(0)])]);
        let err = parse_multicall_response(response, 5, "d.stop").unwrap_err();
        assert!(err.to_string().contains("returned 1 results for 5 calls"));
    }

    #[test]
    fn empty_input_short_circuits_without_a_request() {
        let response = XmlValue::Array(vec![]);
        let results = parse_multicall_response(response, 0, "d.stop").unwrap();
        assert!(results.is_empty());
    }

    fn xmlrpc_response(value: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?><methodResponse><params><param><value>{value}</value></param></params></methodResponse>"
        )
    }

    #[test]
    fn xml_parser_rejects_oversized_arrays() {
        let item = "<value><int>0</int></value>";
        let items = item.repeat(MAX_XMLRPC_COLLECTION_ITEMS + 1);
        let xml = xmlrpc_response(&format!("<array><data>{items}</data></array>"));

        let error = parse_xmlrpc_response(xml.as_bytes()).unwrap_err();
        assert!(error
            .to_string()
            .contains("XML-RPC array exceeds 16384 items"));
    }

    #[test]
    fn xml_parser_rejects_excessive_nesting() {
        let levels = MAX_XMLRPC_VALUE_DEPTH + 1;
        let mut value = String::from("<array><data><value>");
        for _ in 1..levels {
            value.push_str("<array><data><value>");
        }
        value.push_str("<int>0</int>");
        for _ in 0..levels {
            value.push_str("</value></data></array>");
        }
        let xml = xmlrpc_response(&value);

        let error = parse_xmlrpc_response(xml.as_bytes()).unwrap_err();
        assert!(error
            .to_string()
            .contains("XML-RPC value nesting exceeds 64 levels"));
    }

    #[test]
    fn json_rpc_conversion_rejects_oversized_collections_and_nesting() {
        let items = Value::Array(
            (0..=MAX_XMLRPC_COLLECTION_ITEMS)
                .map(|_| Value::from(0))
                .collect(),
        );
        let error = json_to_xml(items).unwrap_err();
        assert!(error
            .to_string()
            .contains("JSON-RPC array exceeds 16384 items"));

        let error = json_to_xml(Value::String("x".repeat(MAX_XMLRPC_TEXT_BYTES + 1))).unwrap_err();
        assert!(error
            .to_string()
            .contains("JSON-RPC text exceeds 16777216 byte limit"));

        let mut nested = Value::from(0);
        for _ in 0..=MAX_XMLRPC_VALUE_DEPTH {
            nested = Value::Array(vec![nested]);
        }
        let error = json_to_xml(nested).unwrap_err();
        assert!(error
            .to_string()
            .contains("JSON-RPC value nesting exceeds 64 levels"));
    }

    #[test]
    fn xml_builder_rejects_oversized_collections() {
        let args = [XmlValue::Array(vec![
            XmlValue::Int(0);
            MAX_XMLRPC_COLLECTION_ITEMS + 1
        ])];
        let error = build_xmlrpc_request("d.multicall2", &args).unwrap_err();
        assert!(error
            .to_string()
            .contains("XML-RPC array exceeds 16384 items"));
    }
}

#[cfg(all(test, unix))]
mod circuit_breaker_tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn background_timeout_opens_circuit_before_returning() {
        let directory = tempfile::tempdir().unwrap();
        let socket_path = directory.path().join("rpc.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        });
        let client = Client::new_unix(socket_path.to_str().unwrap(), 1);

        let timeout = client
            .call_with_priority("d.name", &[], RpcPriority::Background)
            .await
            .expect_err("silent SCGI server should time out");
        assert!(is_timeout_error(&timeout));

        let next = client
            .call_with_priority("d.name", &[], RpcPriority::Background)
            .await
            .expect_err("circuit should be open before timeout returns");
        assert_eq!(next.to_string(), "rTorrent RPC circuit breaker is open");

        server.abort();
        let _ = server.await;
    }
}
