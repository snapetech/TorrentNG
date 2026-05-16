use anyhow::{anyhow, bail, Context, Result};
use bytes::{BufMut, BytesMut};
use quick_xml::{events::Event, name::QName, Reader};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};

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
        })
    }

    /// Construct a Unix-socket client without a full config — for tests.
    pub fn new_unix(socket_path: &str, timeout_secs: u64) -> Self {
        Self {
            transport: Transport::Unix(socket_path.to_owned()),
            timeout: std::time::Duration::from_secs(timeout_secs),
        }
    }

    /// Execute a single XMLRPC method and return the parsed result.
    pub async fn call(&self, method: &str, args: &[XmlValue]) -> Result<XmlValue> {
        let body = build_xmlrpc_request(method, args);
        let response = self
            .scgi_roundtrip(body.as_bytes())
            .await
            .with_context(|| format!("XMLRPC call {method}"))?;
        parse_xmlrpc_response(&response)
    }

    /// Send raw SCGI request and return the HTTP body.
    async fn scgi_roundtrip(&self, xmlrpc_body: &[u8]) -> Result<Vec<u8>> {
        let content_length = xmlrpc_body.len();
        let headers = format!(
            "CONTENT_LENGTH\0{content_length}\0SCGI\01\0REQUEST_METHOD\0POST\0\
             REQUEST_URI\0/RPC2\0CONTENT_TYPE\0text/xml\0"
        );
        let netstring = format!("{}:{},", headers.len(), headers);

        let mut packet = BytesMut::with_capacity(netstring.len() + xmlrpc_body.len());
        packet.put(netstring.as_bytes());
        packet.put(xmlrpc_body);

        let response = tokio::time::timeout(self.timeout, async {
            match &self.transport {
                Transport::Unix(path) => {
                    let mut stream = UnixStream::connect(path)
                        .await
                        .with_context(|| format!("connect to SCGI socket {path}"))?;
                    stream.write_all(&packet).await?;
                    let mut buf = Vec::new();
                    stream.read_to_end(&mut buf).await?;
                    Ok::<_, anyhow::Error>(buf)
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
                b"fault" => bail!("XMLRPC fault returned by rTorrent"),
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
