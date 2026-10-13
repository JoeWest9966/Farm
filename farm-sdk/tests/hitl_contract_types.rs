//! HITL 操作面的契约形状（ticket #22）：release / follow_up / clear_queue。
//!
//! 面板作为契约的**另一个客户端**（ADR-0002 §1）要执行 claim/release、steer/follow_up、
//! abort、clear_queue——其中 release 与两条 agent-runner 注入路由在契约里本来没有位置，
//! 故本票在 `farm-sdk` **加法**补上（不改名任何既有路由/事件，老客户端忽略即可）。

use farm_sdk::routes::CONTRACT_ROUTES;
use farm_sdk::services::agent_runner::{ClearQueueResponse, SteerRequest, SteerResponse};
use farm_sdk::services::tracker::{ReleaseOutcome, ReleaseRequest, ReleaseResponse};

#[test]
fn the_release_route_lives_next_to_the_claim_route_under_the_tracker_prefix() {
    let route = CONTRACT_ROUTES
        .iter()
        .find(|r| r.path == "/v1/tracker/releases")
        .expect("release 必须是契约路由（面板与编排器同路）");
    assert_eq!(route.method, "POST");
    assert!(
        route.path.starts_with("/v1/tracker/"),
        "release 住在 tracker 服务前缀下：{}",
        route.path
    );
    assert!(!route.summary.is_empty(), "每条路由都要有一句话说明");
}

#[test]
fn the_follow_up_and_clear_queue_routes_live_next_to_steer_and_abort() {
    for path in [
        "/v1/agent-runner/attempts/{attempt_id}/follow-up",
        "/v1/agent-runner/attempts/{attempt_id}/clear-queue",
    ] {
        let route = CONTRACT_ROUTES
            .iter()
            .find(|r| r.path == path)
            .expect("注入路由必须与 steer/abort 同在契约路由表里");
        assert_eq!(route.method, "POST", "{path}");
        assert!(
            route.path.starts_with("/v1/agent-runner/attempts/{attempt_id}/"),
            "attempt_id 以路径为准（同 steer/abort）：{path}"
        );
    }
}

#[test]
fn a_release_request_names_the_ticket_and_the_claimant_handing_it_back() {
    let request = ReleaseRequest {
        issue_id: "22".into(),
        assignee: "panel".into(),
    };

    let json = serde_json::to_string(&request).expect("serialize");
    let back: ReleaseRequest = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(back, request);
    assert!(json.contains(r#""issue_id":"22""#), "{json}");
}

#[test]
fn a_release_response_tells_the_panel_whether_it_actually_held_the_ticket() {
    let released = ReleaseResponse {
        issue_id: "22".into(),
        outcome: ReleaseOutcome::Released,
    };
    let idempotent = ReleaseResponse {
        issue_id: "22".into(),
        outcome: ReleaseOutcome::NotClaimed,
    };

    let json = serde_json::to_string(&released).expect("serialize");
    assert!(json.contains(r#""outcome":"released""#), "{json}");
    assert_eq!(
        serde_json::from_str::<ReleaseResponse>(&json).expect("deserialize"),
        released
    );

    let json = serde_json::to_string(&idempotent).expect("serialize");
    assert!(json.contains(r#""outcome":"not_claimed""#), "{json}");
    assert_ne!(idempotent.outcome, released.outcome);
}

#[test]
fn the_release_outcome_is_a_closed_set() {
    // 闭集：只有「真的交还了」与「本来就没持有（幂等空操作）」两种成功投影；
    // 持着票的是别人 → 闭集 conflict 错误信封，不进这个枚举。
    assert_eq!(ReleaseOutcome::ALL.len(), 2);
    assert_eq!(ReleaseOutcome::ALL[0].as_str(), "released");
    assert_eq!(ReleaseOutcome::ALL[1].as_str(), "not_claimed");
}

#[test]
fn a_release_request_still_rejects_unknown_fields() {
    let body = r#"{"issue_id":"22","assignee":"panel","holder":"someone-else"}"#;

    let err = serde_json::from_str::<ReleaseRequest>(body)
        .expect_err("deny_unknown_fields：多出来的字段必须被拒");

    assert!(err.is_data(), "schema 拒绝而非 IO 错误：{err}");
}

#[test]
fn clear_queue_reports_how_many_pending_injections_it_dropped() {
    let response = ClearQueueResponse {
        attempt_id: "att-22-1".into(),
        cleared: 2,
    };

    let json = serde_json::to_string(&response).expect("serialize");
    let back: ClearQueueResponse = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(back, response);
    assert_eq!(back.cleared, 2);
    assert!(json.contains(r#""cleared":2"#), "{json}");
}

#[test]
fn follow_up_reuses_the_steer_request_and_response_shapes() {
    // follow_up 与 steer 的受理形状同构（`{message}` → `{attempt_id, state, disposition}`）：
    // 复用契约类型，不另立一套信封。
    let request = SteerRequest {
        attempt_id: "att-22-1".into(),
        message: "also update the docs".into(),
    };
    let response = SteerResponse {
        attempt_id: "att-22-1".into(),
        state: farm_sdk::services::agent_runner::AttemptState::Running,
        disposition: Some(farm_sdk::services::agent_runner::Disposition::DEFERRED.into()),
    };

    let request_json = serde_json::to_string(&request).expect("serialize");
    let response_json = serde_json::to_string(&response).expect("serialize");

    assert!(request_json.contains(r#""message":"also update the docs""#), "{request_json}");
    assert!(response_json.contains(r#""disposition":"deferred""#), "{response_json}");
    assert!(
        serde_json::from_str::<SteerRequest>(&request_json).is_ok(),
        "follow_up 请求体就是 steer 请求体"
    );
}
