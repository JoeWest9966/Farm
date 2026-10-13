//! 契约版本握手（contract version handshake）：编排器接入第一跳。

use farm_sdk::handshake::{is_compatible, CONTRACT_MAJOR, CONTRACT_VERSION};
use farm_sdk::{ClientInfo, ClientKind, HandshakeRequest, HandshakeResponse};

#[test]
fn host_declares_a_semver_contract_version() {
    let parts: Vec<&str> = CONTRACT_VERSION.split('.').collect();
    assert_eq!(parts.len(), 3, "契约版本必须是 MAJOR.MINOR.PATCH");
    assert_eq!(
        parts[0].parse::<u32>().ok(),
        Some(CONTRACT_MAJOR),
        "CONTRACT_MAJOR 必须与版本串主版本号一致"
    );
}

#[test]
fn orchestrator_declares_itself_as_an_orchestrator_client() {
    let req = HandshakeRequest {
        contract_version: CONTRACT_VERSION.to_string(),
        client: ClientInfo {
            name: "orchestrator-skeleton".into(),
            version: "0.1.0".into(),
            kind: ClientKind::Orchestrator,
        },
    };
    let json = serde_json::to_string(&req).expect("serialize");
    assert!(json.contains("\"kind\":\"orchestrator\""), "client kind 是闭集字符串");
}

#[test]
fn same_major_version_is_compatible() {
    assert!(is_compatible("1.0.0"));
    assert!(is_compatible("1.4.9"));
    assert!(is_compatible("1.0"));
}

#[test]
fn different_major_version_is_rejected() {
    assert!(!is_compatible("2.0.0"));
    assert!(!is_compatible("0.9.0"));
}

#[test]
fn malformed_version_is_rejected() {
    assert!(!is_compatible(""));
    assert!(!is_compatible("garbage"));
    assert!(!is_compatible("one.two.three"));
}

#[test]
fn handshake_response_lists_the_five_services_and_the_event_closed_set() {
    // 独立已知清单：v1 五服务（ADR-0001）+ 事件闭集。
    let resp = HandshakeResponse::for_host("0.1.0");
    let services: Vec<&str> = resp.services.iter().map(|s| s.as_str()).collect();
    assert_eq!(services, ["tracker", "agent-runner", "merge", "workspace", "events"]);
    let kinds: Vec<&str> = resp.event_kinds.iter().map(|k| k.as_str()).collect();
    assert!(kinds.contains(&"dispatch"), "事件闭集必含 dispatch");
    assert!(kinds.contains(&"blocked"), "事件闭集必含 blocked（一等事件）");
    assert!(kinds.contains(&"needs_attention"), "事件闭集必含 needs_attention");
    assert_eq!(resp.contract_version, CONTRACT_VERSION);
}

#[test]
fn handshake_response_advertises_the_token_transport_for_both_faces() {
    let resp = HandshakeResponse::for_host("0.1.0");
    assert_eq!(resp.auth.http_header, "X-Farm-Token");
    assert!(resp.auth.also_accepts_bearer);
    assert!(resp.auth.ws_upgrade_requires_token, "WS 握手同样验 token");
    assert_eq!(resp.auth.admin_header, "X-Farm-Admin-Token");
}
