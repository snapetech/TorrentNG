use std::sync::Arc;

use rt_api_rtorrent::{execute_xml, execute_xml_with_token, supported_methods, AppState};
use rt_session::SessionRegistry;
use tokio::sync::RwLock;

fn method_call(method: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><methodCall><methodName>{method}</methodName><params/></methodCall>"#
    )
}

#[tokio::test]
async fn public_library_entry_point_executes_xmlrpc_without_http_server() {
    let state = AppState::new(Arc::new(RwLock::new(SessionRegistry::new())));
    let response = execute_xml(&state, &method_call("system.library_version")).await;

    assert!(response.contains("<string>native</string>"), "{response}");
    assert!(supported_methods().contains(&"d.multicall"));
}

#[tokio::test]
async fn public_library_entry_point_enforces_embedded_credentials() {
    let state = AppState::new(Arc::new(RwLock::new(SessionRegistry::new())))
        .with_tokens(vec!["library-secret".to_owned()]);
    let request = method_call("system.client_version");

    assert!(execute_xml(&state, &request).await.contains("401"));
    assert!(execute_xml_with_token(&state, &request, Some("wrong"))
        .await
        .contains("401"));
    assert!(
        execute_xml_with_token(&state, &request, Some("library-secret"))
            .await
            .contains("<string>TorrentNG</string>")
    );
}
