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

#[derive(Debug, Clone)]
pub enum Transport {
    Unix(String),
    Tcp(String),
}

#[derive(Clone)]
pub struct Client {
    transport: Transport,
    timeout: std::time::Duration,
    rpc_gate: Arc<Semaphore>,
    low_priority_pause_until: Arc<Mutex<Option<Instant>>>,
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
            rpc_gate: Arc::new(Semaphore::new(1)),
            low_priority_pause_until: Arc::new(Mutex::new(None)),
        })
    }

    /// Construct a Unix-socket client without a full config — for tests.
    pub fn new_unix(socket_path: &str, timeout_secs: u64) -> Self {
        Self {
            transport: Transport::Unix(socket_path.to_owned()),
            timeout: std::time::Duration::from_secs(timeout_secs),
            rpc_gate: Arc::new(Semaphore::new(1)),
            low_priority_pause_until: Arc::new(Mutex::new(None)),
        }
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

    pub async fn call_sync(&self, method: &str, args: &[XmlValue]) -> Result<XmlValue> {
        self.call_with_priority(method, args, RpcPriority::Background)
            .await
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

        match self.call_json(method, args).await {
            Ok(value) => Ok(value),
            Err(json_err) => {
                if is_jsonrpc_unavailable(&json_err) {
                    self.call_xml(method, args).await
                } else {
                    Err(json_err)
                }
            }
        }
        .inspect_err(|e| {
            if priority == RpcPriority::Background && is_timeout_error(e) {
                let pause = self.low_priority_pause_until.clone();
                tokio::spawn(async move {
                    *pause.lock().await = Some(Instant::now() + std::time::Duration::from_secs(15));
                });
            }
        })
    }

    async fn call_json(&self, method: &str, args: &[XmlValue]) -> Result<XmlValue> {
        if args.iter().any(contains_base64) {
            bail!("JSON-RPC unavailable for XML-RPC base64 payload");
        }
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": args.iter().map(xml_to_json).collect::<Vec<_>>(),
        })
        .to_string();
        let response = self
            .scgi_roundtrip("application/json", body.as_bytes())
            .await
            .with_context(|| format!("JSON-RPC call {method}"))?;
        parse_jsonrpc_response(&response)
    }

    async fn call_xml(&self, method: &str, args: &[XmlValue]) -> Result<XmlValue> {
        let body = build_xmlrpc_request(method, args);
        let response = self
            .scgi_roundtrip("text/xml", body.as_bytes())
            .await
            .with_context(|| format!("XMLRPC call {method}"))?;
        parse_xmlrpc_response(&response)
    }

    /// Send raw SCGI request and return the HTTP body.
    async fn scgi_roundtrip(&self, content_type: &str, body: &[u8]) -> Result<Vec<u8>> {
        let content_length = body.len();
        let headers = format!(
            "CONTENT_LENGTH\0{content_length}\0SCGI\01\0REQUEST_METHOD\0POST\0\
             REQUEST_URI\0/RPC2\0CONTENT_TYPE\0{content_type}\0"
        );
        let netstring = format!("{}:{},", headers.len(), headers);

        let mut packet = BytesMut::with_capacity(netstring.len() + body.len());
        packet.put(netstring.as_bytes());
        packet.put(body);

        let response = tokio::time::timeout(self.timeout, async {
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
                        stream.read_to_end(&mut buf).await?;
                        Ok::<_, anyhow::Error>(buf)
                    }
                }
                Transport::Tcp(addr) => {
                    let mut stream = TcpStream::connect(addr)
                        .await
                        .with_context(|| format!("connect to SCGI addr {addr}"))?;
                    stream.write_all(&packet).await?;
                    let mut buf = Vec::new();
                    stream.read_to_end(&mut buf).await?;
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

fn json_to_xml(value: Value) -> XmlValue {
    match value {
        Value::Null => XmlValue::Nil,
        Value::Bool(b) => XmlValue::Bool(b),
        Value::Number(n) => XmlValue::Int(
            n.as_i64()
                .or_else(|| n.as_u64().map(|n| n as i64))
                .unwrap_or(0),
        ),
        Value::String(s) => XmlValue::String(s),
        Value::Array(items) => XmlValue::Array(items.into_iter().map(json_to_xml).collect()),
        Value::Object(map) => XmlValue::Struct(
            map.into_iter()
                .map(|(key, value)| (key, json_to_xml(value)))
                .collect(),
        ),
    }
}

fn parse_jsonrpc_response(body: &[u8]) -> Result<XmlValue> {
    let value: Value =
        serde_json::from_slice(body).context("JSON-RPC response is not valid JSON")?;
    if let Some(error) = value.get("error") {
        bail!(
            "JSON-RPC error: {}",
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        );
    }
    Ok(json_to_xml(
        value.get("result").cloned().unwrap_or(Value::Null),
    ))
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

fn build_xmlrpc_request(method: &str, args: &[XmlValue]) -> String {
    let mut out = String::from("<?xml version=\"1.0\"?>\n<methodCall>\n");
    out.push_str(&format!(
        "  <methodName>{}</methodName>\n  <params>\n",
        xml_escape(method)
    ));
    for arg in args {
        out.push_str("    <param><value>");
        write_xml_value(&mut out, arg);
        out.push_str("</value></param>\n");
    }
    out.push_str("  </params>\n</methodCall>");
    out
}

fn write_xml_value(out: &mut String, v: &XmlValue) {
    match v {
        XmlValue::String(s) => {
            out.push_str("<string>");
            out.push_str(&xml_escape(s));
            out.push_str("</string>");
        }
        XmlValue::Base64(s) => {
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
            out.push_str("<array><data>");
            for item in items {
                out.push_str("<value>");
                write_xml_value(out, item);
                out.push_str("</value>");
            }
            out.push_str("</data></array>");
        }
        XmlValue::Struct(fields) => {
            out.push_str("<struct>");
            for (k, v) in fields {
                out.push_str(&format!("<member><name>{}</name><value>", xml_escape(k)));
                write_xml_value(out, v);
                out.push_str("</value></member>");
            }
            out.push_str("</struct>");
        }
        XmlValue::Nil => {
            out.push_str("<nil/>");
        }
    }
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

fn parse_xmlrpc_response(xml: &[u8]) -> Result<XmlValue> {
    let xml_str = std::str::from_utf8(xml).context("XMLRPC response not UTF-8")?;
    let mut reader = Reader::from_str(xml_str);
    reader.config_mut().trim_text(true);

    // Seek to <methodResponse>
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.name().as_ref() == b"methodResponse" => break,
            Event::Eof => bail!("XMLRPC response missing <methodResponse>"),
            _ => {}
        }
    }

    // Check for <fault> vs <params>
    loop {
        match reader.read_event()? {
            Event::Start(e) => match e.name().as_ref() {
                b"fault" => {
                    let fault = parse_value(&mut reader)?;
                    bail!(
                        "XMLRPC fault returned by rTorrent: {}",
                        describe_xml_value(&fault)
                    )
                }
                b"params" => break,
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
            Event::Start(e) if e.name().as_ref() == b"value" => break,
            Event::End(_) | Event::Eof => return Ok(XmlValue::Nil),
            _ => {}
        }
    }
    parse_value_content(reader)
}

fn parse_value_content(reader: &mut Reader<&[u8]>) -> Result<XmlValue> {
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let val = match e.name().as_ref() {
                    b"string" => {
                        let text = read_text_string(reader, e.name())?;
                        XmlValue::String(text)
                    }
                    b"int" | b"i4" | b"i8" => {
                        let text = read_text_string(reader, e.name())?;
                        XmlValue::Int(text.trim().parse().unwrap_or(0))
                    }
                    b"boolean" => {
                        let text = read_text_string(reader, e.name())?;
                        XmlValue::Bool(text.trim() == "1")
                    }
                    b"array" => parse_array(reader)?,
                    b"struct" => parse_struct(reader)?,
                    b"nil" => {
                        let _ = reader.read_text(e.name());
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
                let s = t.decode()?.trim().to_owned();
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

fn parse_array(reader: &mut Reader<&[u8]>) -> Result<XmlValue> {
    let mut items = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.name().as_ref() == b"value" => {
                items.push(parse_value_content(reader)?);
            }
            Event::End(e) if e.name().as_ref() == b"array" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(XmlValue::Array(items))
}

fn parse_struct(reader: &mut Reader<&[u8]>) -> Result<XmlValue> {
    let mut fields = Vec::new();
    let mut current_name = String::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) => match e.name().as_ref() {
                b"name" => {
                    current_name = read_text_string(reader, e.name())?;
                }
                b"value" => {
                    let val = parse_value_content(reader)?;
                    fields.push((std::mem::take(&mut current_name), val));
                }
                _ => {}
            },
            Event::End(e) if e.name().as_ref() == b"struct" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(XmlValue::Struct(fields))
}

fn read_text_string(reader: &mut Reader<&[u8]>, end: QName<'_>) -> Result<String> {
    Ok(reader.read_text(end)?.decode()?.into_owned())
}
