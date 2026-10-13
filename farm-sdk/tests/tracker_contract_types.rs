//! Tracker 契约类型的加法兼容性（ticket #13）。
//!
//! `IssueView` 长出 blocking 边字段是**加法**契约变更：老客户端与老快照发的 JSON
//! 必须照旧解析，新字段缺省为空。闭集错误码、路由路径一律不动（ADR-0001 决策 6）。

use farm_sdk::services::tracker::{
    ClaimRequest, ClaimResponse, IssueListResponse, IssueState, IssueView,
};

/// 一张带 blocking 边的归一化票视图。
fn view_with_edges() -> IssueView {
    IssueView {
        issue_id: "13".into(),
        title: "tracker service".into(),
        state: IssueState::Claimed,
        labels: vec!["tracker".into()],
        assignee: Some("orchestrator-a".into()),
        updated_at: Some("2026-01-01T00:00:00Z".into()),
        blocked_by: vec!["1".into(), "2".into()],
        blocking: vec![],
    }
}

#[test]
fn an_issue_view_without_the_new_blocking_fields_still_deserializes() {
    // 老客户端/老快照的 JSON：没有 blocked_by / blocking 也必须能读（加法兼容）。
    let old = r#"{"issue_id":"13","title":"tracker","state":"open","labels":["tracker"],
                 "assignee":null,"updated_at":"2026-01-01T00:00:00Z"}"#;

    let view: IssueView = serde_json::from_str(old).expect("老形状必须仍可解析");

    assert_eq!(view.issue_id, "13");
    assert_eq!(view.state, IssueState::Open);
    assert!(view.blocked_by.is_empty(), "缺省即无边");
    assert!(view.blocking.is_empty(), "缺省即无边");
}

#[test]
fn blocking_edges_round_trip_through_the_contract_view() {
    let view = view_with_edges();

    let json = serde_json::to_string(&view).expect("serialize");
    assert!(
        json.contains(r#""blocked_by":["1","2"]"#),
        "blocking 边必须过契约面：{json}"
    );
    let back: IssueView = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, view);
}

#[test]
fn an_issue_view_without_edges_stays_free_of_empty_arrays() {
    let view = IssueView {
        blocked_by: vec![],
        blocking: vec![],
        ..view_with_edges()
    };

    let json = serde_json::to_string(&view).expect("serialize");

    assert!(!json.contains("blocked_by"), "空边不占字节：{json}");
    assert!(!json.contains("blocking"), "空边不占字节：{json}");
}

#[test]
fn the_issue_list_response_keeps_the_skeleton_envelope_shape() {
    let response = IssueListResponse { issues: vec![] };

    let json = serde_json::to_string(&response).expect("serialize");

    assert_eq!(json, r#"{"issues":[]}"#, "信封形状不得变");
}

#[test]
fn a_claim_response_carries_the_winning_assignee() {
    let response = ClaimResponse {
        issue_id: "13".into(),
        claimed: true,
        holder: Some("orchestrator-a".into()),
    };

    let json = serde_json::to_string(&response).expect("serialize");
    let back: ClaimResponse = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(back, response);
    assert!(json.contains(r#""claimed":true"#), "{json}");
}

#[test]
fn a_claim_request_still_rejects_unknown_fields() {
    let body = r#"{"issue_id":"13","assignee":"orchestrator-a","holder":"someone-else"}"#;

    let err = serde_json::from_str::<ClaimRequest>(body)
        .expect_err("deny_unknown_fields：多出来的字段必须被拒");

    assert!(err.is_data(), "schema 拒绝而非 IO 错误：{err}");
}
