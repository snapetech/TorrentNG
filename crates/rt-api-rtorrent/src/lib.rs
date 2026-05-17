use std::{collections::BTreeMap, sync::Arc};

use base64::{engine::general_purpose, Engine as _};
use rt_engine::EngineHandle;
use rt_metainfo::{parse_magnet, parse_torrent};
use rt_metrics::{MemoryClass, MemoryLease};
use rt_session::{SessionRegistry, TorrentEntry};
use serde_json::Value;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
    pub session_path: String,
    pub network_port: i64,
    custom: Arc<RwLock<BTreeMap<String, BTreeMap<String, RtValue>>>>,
}

impl AppState {
    pub fn new(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        Self {
            registry,
            engine: None,
            session_path: String::new(),
            network_port: 0,
            custom: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    pub fn with_engine(registry: Arc<RwLock<SessionRegistry>>, engine: EngineHandle) -> Self {
        Self {
            engine: Some(engine),
            ..Self::new(registry)
        }
    }
}

async fn reserve_rtorrent_api_snapshot(
    state: &AppState,
    bytes: u64,
) -> Result<Option<MemoryLease>, String> {
    let Some(engine) = &state.engine else {
        return Ok(None);
    };
    engine
        .reserve_memory(MemoryClass::ApiSnapshot, bytes)
        .await?
        .map(Some)
        .ok_or_else(|| "api snapshot memory budget exhausted".to_owned())
}

fn estimate_rtorrent_multicall_snapshot_bytes(torrent_count: usize, command_count: usize) -> u64 {
    let commands = command_count.max(1) as u64;
    8 * 1024 + (torrent_count as u64).saturating_mul(512 + commands.saturating_mul(160))
}

#[derive(Debug, Clone, PartialEq)]
pub enum RtValue {
    Int(i64),
    Bool(bool),
    String(String),
    Array(Vec<RtValue>),
    Struct(BTreeMap<String, RtValue>),
    Nil,
}

impl RtValue {
    fn as_str(&self) -> Option<&str> {
        match self {
            RtValue::String(value) => Some(value),
            _ => None,
        }
    }
}

pub fn supported_methods() -> &'static [&'static str] {
    &[
        "method.list",
        "system.client_version",
        "system.library_version",
        "system.time",
        "session.name",
        "session.path",
        "network.port_open",
        "network.port_random",
        "throttle.global_down.max_rate",
        "throttle.global_up.max_rate",
        "view.list",
        "view.size",
        "d.hash",
        "d.name",
        "d.base_path",
        "d.directory",
        "d.size_bytes",
        "d.left_bytes",
        "d.completed_bytes",
        "d.complete",
        "d.is_active",
        "d.state",
        "d.state_changed",
        "d.up.total",
        "d.down.total",
        "d.ratio",
        "d.custom",
        "d.custom.set",
        "d.multicall",
        "d.multicall2",
        "load.normal",
        "load.start",
        "load.raw",
        "load.raw_start",
        "d.erase",
        "d.pause",
        "d.resume",
        "d.stop",
        "d.start",
        "d.tracker_announce",
        "f.multicall",
        "t.multicall",
        "p.multicall",
    ]
}

pub async fn execute(
    state: &AppState,
    method: &str,
    params: &[RtValue],
) -> Result<RtValue, String> {
    match method {
        "method.list" => Ok(RtValue::Array(
            supported_methods()
                .iter()
                .map(|method| RtValue::String((*method).to_owned()))
                .collect(),
        )),
        "system.client_version" => Ok(RtValue::String("TorrentNG".to_owned())),
        "system.library_version" => Ok(RtValue::String("native".to_owned())),
        "system.time" => Ok(RtValue::Int(unix_now())),
        "session.name" => Ok(RtValue::String("TorrentNG".to_owned())),
        "session.path" => Ok(RtValue::String(state.session_path.clone())),
        "network.port_open" => Ok(RtValue::Int(state.network_port)),
        "network.port_random" => Ok(RtValue::Bool(false)),
        "throttle.global_down.max_rate" | "throttle.global_up.max_rate" => Ok(RtValue::Int(0)),
        "view.list" => Ok(RtValue::Array(vec![RtValue::String("main".to_owned())])),
        "view.size" => Ok(RtValue::Int(state.registry.read().await.len() as i64)),
        "d.multicall" | "d.multicall2" => d_multicall(state, params).await,
        "load.normal" | "load.start" | "load.raw" | "load.raw_start" => {
            load(state, method, params).await
        }
        "d.erase" => lifecycle(state, params, Lifecycle::Erase).await,
        "d.pause" | "d.stop" => lifecycle(state, params, Lifecycle::Pause).await,
        "d.resume" | "d.start" => lifecycle(state, params, Lifecycle::Resume).await,
        "d.tracker_announce" => Ok(RtValue::Int(0)),
        "f.multicall" => Ok(RtValue::Array(vec![RtValue::Array(vec![
            RtValue::String(String::new()),
            RtValue::Int(0),
            RtValue::Int(1),
        ])])),
        "t.multicall" | "p.multicall" => Ok(RtValue::Array(Vec::new())),
        _ if method.starts_with("d.") => d_read_or_write(state, method, params).await,
        _ => Err(format!("unsupported rTorrent XMLRPC method {method}")),
    }
}

pub async fn execute_xml(state: &AppState, request: &str) -> String {
    match parse_method_call(request) {
        Ok((method, params)) => match execute(state, &method, &params).await {
            Ok(value) => method_response(&value),
            Err(message) => fault_response(1, &message),
        },
        Err(message) => fault_response(1, &message),
    }
}

async fn d_read_or_write(
    state: &AppState,
    method: &str,
    params: &[RtValue],
) -> Result<RtValue, String> {
    let hash = params
        .first()
        .and_then(RtValue::as_str)
        .ok_or_else(|| format!("{method} requires info hash"))?;
    if method == "d.custom.set" {
        let key = params.get(1).and_then(RtValue::as_str).unwrap_or_default();
        let value = params
            .get(2)
            .cloned()
            .unwrap_or(RtValue::String(String::new()));
        state
            .custom
            .write()
            .await
            .entry(hash.to_owned())
            .or_default()
            .insert(key.to_owned(), value);
        return Ok(RtValue::Int(0));
    }
    let registry = state.registry.read().await;
    let entry = registry
        .get(hash)
        .ok_or_else(|| format!("torrent not found: {hash}"))?;
    Ok(project_download_field(
        entry,
        method,
        state.custom.read().await.get(hash),
        params.get(1).and_then(RtValue::as_str),
    ))
}

fn project_download_field(
    entry: &TorrentEntry,
    method: &str,
    custom: Option<&BTreeMap<String, RtValue>>,
    custom_key: Option<&str>,
) -> RtValue {
    match method {
        "d.hash" => RtValue::String(entry.info_hash.clone()),
        "d.name" => RtValue::String(entry.name.clone()),
        "d.base_path" | "d.directory" => RtValue::String(entry.save_path.clone()),
        "d.size_bytes" => RtValue::Int(entry.total_length as i64),
        "d.left_bytes" => RtValue::Int(entry.amount_left as i64),
        "d.completed_bytes" => {
            RtValue::Int(entry.total_length.saturating_sub(entry.amount_left) as i64)
        }
        "d.complete" => RtValue::Bool(entry.total_length > 0 && entry.amount_left == 0),
        "d.is_active" => RtValue::Bool(matches!(
            entry.state.as_str(),
            "downloading" | "seeding" | "checking"
        )),
        "d.state" => RtValue::String(entry.state.as_str().to_owned()),
        "d.state_changed" => RtValue::Int(entry.added_at as i64),
        "d.up.total" => RtValue::Int(entry.stats.uploaded as i64),
        "d.down.total" => RtValue::Int(entry.stats.downloaded as i64),
        "d.ratio" => RtValue::Int((entry.stats.ratio() * 1000.0).round() as i64),
        "d.custom" => custom
            .and_then(|values| custom_key.and_then(|key| values.get(key)))
            .cloned()
            .unwrap_or_else(|| RtValue::String(String::new())),
        _ => RtValue::Nil,
    }
}

async fn d_multicall(state: &AppState, params: &[RtValue]) -> Result<RtValue, String> {
    let commands = params
        .iter()
        .skip(1)
        .filter_map(RtValue::as_str)
        .map(|command| command.trim_end_matches('=').to_owned())
        .collect::<Vec<_>>();
    let torrent_count = state.registry.read().await.len();
    let _lease = reserve_rtorrent_api_snapshot(
        state,
        estimate_rtorrent_multicall_snapshot_bytes(torrent_count, commands.len()),
    )
    .await?;
    let registry = state.registry.read().await;
    let custom = state.custom.read().await;
    let mut rows = Vec::new();
    for entry in registry.iter() {
        let row = commands
            .iter()
            .map(|command| {
                project_download_field(entry, command, custom.get(&entry.info_hash), None)
            })
            .collect();
        rows.push(RtValue::Array(row));
    }
    Ok(RtValue::Array(rows))
}

async fn load(state: &AppState, method: &str, params: &[RtValue]) -> Result<RtValue, String> {
    let payload = params
        .first()
        .and_then(RtValue::as_str)
        .ok_or_else(|| "load requires magnet URI, torrent bytes, or path".to_owned())?;
    let mut entry = if payload.starts_with("magnet:") {
        let magnet = parse_magnet(payload).map_err(|err| err.to_string())?;
        let hash = magnet
            .info_hash_v1
            .map(hex_lower)
            .or_else(|| magnet.info_hash_v2.map(hex_lower))
            .ok_or_else(|| "magnet missing supported info hash".to_owned())?;
        TorrentEntry::new(
            hash,
            magnet.display_name.unwrap_or_else(|| "magnet".to_owned()),
            String::new(),
        )
    } else if let Some(bytes) = load_torrent_bytes(method, payload) {
        let parsed = parse_torrent(&bytes).map_err(|err| err.to_string())?;
        let hash = parsed
            .v1_info_hash()
            .map(hex_lower)
            .or_else(|| parsed.v2_info_hash().map(hex_lower))
            .ok_or_else(|| "torrent missing supported info hash".to_owned())?;
        TorrentEntry::new(hash, parsed.name().to_owned(), String::new())
    } else {
        return Ok(RtValue::Int(0));
    };
    if method.ends_with("start") {
        let _ = entry.transition(rt_session::TorrentState::Downloading);
    }
    let _ = state.registry.write().await.add(entry);
    Ok(RtValue::Int(0))
}

fn load_torrent_bytes(method: &str, payload: &str) -> Option<Vec<u8>> {
    if method.contains(".raw") {
        return general_purpose::STANDARD.decode(payload).ok();
    }
    std::fs::read(payload).ok()
}

enum Lifecycle {
    Erase,
    Pause,
    Resume,
}

async fn lifecycle(
    state: &AppState,
    params: &[RtValue],
    lifecycle: Lifecycle,
) -> Result<RtValue, String> {
    let hash = params
        .first()
        .and_then(RtValue::as_str)
        .ok_or_else(|| "lifecycle command requires info hash".to_owned())?;
    if let Some(engine) = &state.engine {
        match lifecycle {
            Lifecycle::Erase => {
                let _ = engine.remove_torrent(hash.to_owned(), false).await;
            }
            Lifecycle::Pause => {
                let _ = engine.pause_torrent(hash.to_owned()).await;
            }
            Lifecycle::Resume => {
                let _ = engine.resume_torrent(hash.to_owned()).await;
            }
        }
    }
    if matches!(lifecycle, Lifecycle::Erase) {
        let _ = state.registry.write().await.remove(hash);
    }
    Ok(RtValue::Int(0))
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn hex_lower<const N: usize>(bytes: [u8; N]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_method_call(xml: &str) -> Result<(String, Vec<RtValue>), String> {
    let method = between(xml, "<methodName>", "</methodName>")
        .ok_or_else(|| "XMLRPC request missing methodName".to_owned())?;
    let mut params = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<param>") {
        rest = &rest[start + "<param>".len()..];
        let Some(end) = rest.find("</param>") else {
            break;
        };
        params.push(parse_value(&rest[..end]));
        rest = &rest[end + "</param>".len()..];
    }
    Ok((xml_unescape(method), params))
}

fn parse_value(xml: &str) -> RtValue {
    let xml = xml.trim();
    if xml.starts_with("<value>") && xml.ends_with("</value>") {
        return parse_value(&xml["<value>".len()..xml.len() - "</value>".len()]);
    }
    if let Some(value) = between(xml, "<array>", "</array>") {
        let data = between(value, "<data>", "</data>").unwrap_or(value);
        return RtValue::Array(parse_value_nodes(data));
    }
    if let Some(value) = between(xml, "<struct>", "</struct>") {
        return RtValue::Struct(parse_struct_members(value));
    }
    if let Some(value) = between(xml, "<string>", "</string>") {
        return RtValue::String(xml_unescape(value));
    }
    if let Some(value) = between(xml, "<base64>", "</base64>") {
        return RtValue::String(value.trim().to_owned());
    }
    if let Some(value) = between(xml, "<i4>", "</i4>").or_else(|| between(xml, "<int>", "</int>")) {
        return RtValue::Int(value.trim().parse().unwrap_or_default());
    }
    if let Some(value) = between(xml, "<boolean>", "</boolean>") {
        return RtValue::Bool(value.trim() == "1" || value.trim().eq_ignore_ascii_case("true"));
    }
    if xml.contains("<nil/>") {
        return RtValue::Nil;
    }
    RtValue::String(xml_unescape(xml))
}

fn parse_value_nodes(mut xml: &str) -> Vec<RtValue> {
    let mut values = Vec::new();
    while let Some((value, rest)) = next_value_node(xml) {
        values.push(parse_value(value));
        xml = rest;
    }
    values
}

fn next_value_node(xml: &str) -> Option<(&str, &str)> {
    let open = "<value>";
    let close = "</value>";
    let start = xml.find(open)? + open.len();
    let mut depth = 1usize;
    let mut pos = start;
    while depth > 0 {
        let next_open = xml[pos..].find(open).map(|idx| pos + idx);
        let next_close = xml[pos..].find(close).map(|idx| pos + idx)?;
        if let Some(next_open) = next_open {
            if next_open < next_close {
                depth += 1;
                pos = next_open + open.len();
                continue;
            }
        }
        depth -= 1;
        if depth == 0 {
            let rest = &xml[next_close + close.len()..];
            return Some((&xml[start..next_close], rest));
        }
        pos = next_close + close.len();
    }
    None
}

fn parse_struct_members(mut xml: &str) -> BTreeMap<String, RtValue> {
    let mut values = BTreeMap::new();
    while let Some(start) = xml.find("<member>") {
        xml = &xml[start + "<member>".len()..];
        let Some(end) = xml.find("</member>") else {
            break;
        };
        let member = &xml[..end];
        if let Some(name) = between(member, "<name>", "</name>") {
            let value = between(member, "<value>", "</value>")
                .map(parse_value)
                .unwrap_or(RtValue::Nil);
            values.insert(xml_unescape(name), value);
        }
        xml = &xml[end + "</member>".len()..];
    }
    values
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

fn method_response(value: &RtValue) -> String {
    format!(
        "<?xml version=\"1.0\"?><methodResponse><params><param><value>{}</value></param></params></methodResponse>",
        value_xml(value)
    )
}

fn fault_response(code: i64, message: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?><methodResponse><fault><value><struct><member><name>faultCode</name><value><int>{code}</int></value></member><member><name>faultString</name><value><string>{}</string></value></member></struct></value></fault></methodResponse>",
        xml_escape(message)
    )
}

fn value_xml(value: &RtValue) -> String {
    match value {
        RtValue::Int(value) => format!("<int>{value}</int>"),
        RtValue::Bool(value) => format!("<boolean>{}</boolean>", if *value { 1 } else { 0 }),
        RtValue::String(value) => format!("<string>{}</string>", xml_escape(value)),
        RtValue::Array(values) => format!(
            "<array><data>{}</data></array>",
            values
                .iter()
                .map(|value| format!("<value>{}</value>", value_xml(value)))
                .collect::<String>()
        ),
        RtValue::Struct(values) => format!(
            "<struct>{}</struct>",
            values
                .iter()
                .map(|(key, value)| format!(
                    "<member><name>{}</name><value>{}</value></member>",
                    xml_escape(key),
                    value_xml(value)
                ))
                .collect::<String>()
        ),
        RtValue::Nil => "<nil/>".to_owned(),
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

pub fn value_to_json(value: &RtValue) -> Value {
    match value {
        RtValue::Int(value) => Value::from(*value),
        RtValue::Bool(value) => Value::from(*value),
        RtValue::String(value) => Value::from(value.clone()),
        RtValue::Array(values) => Value::Array(values.iter().map(value_to_json).collect()),
        RtValue::Struct(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), value_to_json(value)))
                .collect(),
        ),
        RtValue::Nil => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rt_session::{TorrentEntry, TorrentState};

    async fn state_with_torrent() -> AppState {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut entry = TorrentEntry::new("a".repeat(40), "alpha".into(), "/data/alpha".into());
            entry.total_length = 100;
            entry.amount_left = 25;
            entry.stats.add_download(75);
            entry.stats.add_upload(150);
            entry.transition(TorrentState::Downloading).unwrap();
            registry.write().await.add(entry).unwrap();
        }
        AppState::new(registry)
    }

    #[test]
    fn method_matrix_advertises_representative_rtorrent_families() {
        let methods = supported_methods();
        for method in [
            "system.client_version",
            "session.path",
            "network.port_open",
            "d.hash",
            "d.multicall2",
            "load.normal",
            "d.erase",
            "d.pause",
            "d.resume",
            "d.tracker_announce",
            "f.multicall",
            "t.multicall",
            "p.multicall",
        ] {
            assert!(methods.contains(&method), "missing {method}");
        }
    }

    #[tokio::test]
    async fn download_reads_project_registry_state() {
        let state = state_with_torrent().await;
        let hash = RtValue::String("a".repeat(40));
        assert_eq!(
            execute(&state, "d.name", &[hash.clone()]).await.unwrap(),
            RtValue::String("alpha".to_owned())
        );
        assert_eq!(
            execute(&state, "d.completed_bytes", &[hash.clone()])
                .await
                .unwrap(),
            RtValue::Int(75)
        );
        assert_eq!(
            execute(&state, "d.ratio", &[hash]).await.unwrap(),
            RtValue::Int(2000)
        );
    }

    #[tokio::test]
    async fn custom_fields_roundtrip() {
        let state = state_with_torrent().await;
        let hash = RtValue::String("a".repeat(40));
        execute(
            &state,
            "d.custom.set",
            &[
                hash.clone(),
                RtValue::String("label".to_owned()),
                RtValue::String("movies".to_owned()),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            execute(
                &state,
                "d.custom",
                &[hash, RtValue::String("label".to_owned())]
            )
            .await
            .unwrap(),
            RtValue::String("movies".to_owned())
        );
    }

    #[tokio::test]
    async fn multicall_returns_rtorrent_row_shape() {
        let state = state_with_torrent().await;
        let value = execute(
            &state,
            "d.multicall2",
            &[
                RtValue::String("main".to_owned()),
                RtValue::String("d.hash=".to_owned()),
                RtValue::String("d.name=".to_owned()),
                RtValue::String("d.left_bytes=".to_owned()),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            value,
            RtValue::Array(vec![RtValue::Array(vec![
                RtValue::String("a".repeat(40)),
                RtValue::String("alpha".to_owned()),
                RtValue::Int(25),
            ])])
        );
    }

    #[tokio::test]
    async fn xmlrpc_fixture_roundtrips() {
        let state = state_with_torrent().await;
        let xml = format!(
            r#"<?xml version="1.0"?><methodCall><methodName>d.name</methodName><params><param><value><string>{}</string></value></param></params></methodCall>"#,
            "a".repeat(40)
        );
        let response = execute_xml(&state, &xml).await;
        assert!(response.contains("<methodResponse>"));
        assert!(response.contains("<string>alpha</string>"));
    }

    #[tokio::test]
    async fn xmlrpc_method_list_and_placeholder_multicalls_have_stable_shapes() {
        let state = state_with_torrent().await;
        let response = execute_xml(
            &state,
            r#"<?xml version="1.0"?><methodCall><methodName>method.list</methodName><params/></methodCall>"#,
        )
        .await;
        assert!(response.contains("<array><data>"));
        assert!(response.contains("<string>d.multicall2</string>"));
        assert!(response.contains("<string>p.multicall</string>"));

        assert_eq!(
            execute(&state, "t.multicall", &[RtValue::String("main".to_owned())])
                .await
                .unwrap(),
            RtValue::Array(Vec::new())
        );
        assert_eq!(
            execute(&state, "p.multicall", &[RtValue::String("main".to_owned())])
                .await
                .unwrap(),
            RtValue::Array(Vec::new())
        );
        assert_eq!(
            execute(&state, "f.multicall", &[RtValue::String("main".to_owned())])
                .await
                .unwrap(),
            RtValue::Array(vec![RtValue::Array(vec![
                RtValue::String(String::new()),
                RtValue::Int(0),
                RtValue::Int(1),
            ])])
        );
    }

    #[test]
    fn xml_value_parser_accepts_nested_arrays_structs_base64_and_nil() {
        let value = parse_value(
            r#"<value><array><data>
              <value><string>alpha</string></value>
              <value><struct>
                <member><name>count</name><value><int>2</int></value></member>
                <member><name>raw</name><value><base64>YWJj</base64></value></member>
                <member><name>empty</name><value><nil/></value></member>
              </struct></value>
            </data></array></value>"#,
        );
        let json = value_to_json(&value);
        assert_eq!(json[0], "alpha");
        assert_eq!(json[1]["count"], 2);
        assert_eq!(json[1]["raw"], "YWJj");
        assert!(json[1]["empty"].is_null());
    }

    #[test]
    fn value_to_json_preserves_rtorrent_types() {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_owned(), RtValue::String("alpha".to_owned()));
        fields.insert("active".to_owned(), RtValue::Bool(true));
        fields.insert("ratio".to_owned(), RtValue::Int(2000));
        let json = value_to_json(&RtValue::Struct(fields));
        assert_eq!(json["name"], "alpha");
        assert_eq!(json["active"], true);
        assert_eq!(json["ratio"], 2000);
    }

    #[tokio::test]
    async fn magnet_load_and_erase_update_registry() {
        let state = AppState::new(Arc::new(RwLock::new(SessionRegistry::new())));
        execute(
            &state,
            "load.normal",
            &[RtValue::String(format!(
                "magnet:?xt=urn:btih:{}&dn=loaded",
                "b".repeat(40)
            ))],
        )
        .await
        .unwrap();
        assert_eq!(state.registry.read().await.len(), 1);
        execute(&state, "d.erase", &[RtValue::String("b".repeat(40))])
            .await
            .unwrap();
        assert_eq!(state.registry.read().await.len(), 0);
    }

    #[test]
    fn xmlrpc_parser_accepts_array_struct_base64_and_nil_shapes() {
        let xml = r#"<value><array><data>
            <value><int>7</int></value>
            <value><boolean>1</boolean></value>
            <value><base64>YWJj</base64></value>
            <value><nil/></value>
            <value><struct><member><name>k</name><value><string>v</string></value></member></struct></value>
        </data></array></value>"#;
        assert_eq!(
            parse_value(xml),
            RtValue::Array(vec![
                RtValue::Int(7),
                RtValue::Bool(true),
                RtValue::String("YWJj".to_owned()),
                RtValue::Nil,
                RtValue::Struct(BTreeMap::from([(
                    "k".to_owned(),
                    RtValue::String("v".to_owned())
                )])),
            ])
        );
    }

    #[tokio::test]
    async fn raw_torrent_load_accepts_xmlrpc_base64_payload() {
        let state = AppState::new(Arc::new(RwLock::new(SessionRegistry::new())));
        let raw = single_file_torrent("raw-test", 4);
        let xml = format!(
            r#"<?xml version="1.0"?><methodCall><methodName>load.raw_start</methodName><params><param><value><base64>{}</base64></value></param></params></methodCall>"#,
            general_purpose::STANDARD.encode(raw)
        );
        let response = execute_xml(&state, &xml).await;
        assert!(response.contains("<int>0</int>"), "{response}");

        let registry = state.registry.read().await;
        assert_eq!(registry.len(), 1);
        let entry = registry.iter().next().unwrap();
        assert_eq!(entry.name, "raw-test");
        assert_eq!(entry.state, TorrentState::Downloading);
    }

    #[test]
    fn rtorrent_api_snapshot_estimate_scales_with_torrents_and_commands() {
        assert_eq!(estimate_rtorrent_multicall_snapshot_bytes(0, 0), 8 * 1024);
        assert_eq!(
            estimate_rtorrent_multicall_snapshot_bytes(10, 0),
            8 * 1024 + 10 * (512 + 160)
        );
        assert!(
            estimate_rtorrent_multicall_snapshot_bytes(10, 20)
                > estimate_rtorrent_multicall_snapshot_bytes(10, 1)
        );
    }

    fn single_file_torrent(name: &str, length: i64) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"d4:infod6:lengthi");
        raw.extend_from_slice(length.to_string().as_bytes());
        raw.extend_from_slice(b"e4:name");
        raw.extend_from_slice(name.len().to_string().as_bytes());
        raw.extend_from_slice(b":");
        raw.extend_from_slice(name.as_bytes());
        raw.extend_from_slice(b"12:piece lengthi16384e6:pieces20:");
        raw.extend_from_slice(&[0_u8; 20]);
        raw.extend_from_slice(b"ee");
        raw
    }
}
