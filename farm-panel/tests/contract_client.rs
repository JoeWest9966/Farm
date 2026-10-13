//! 契约客户端数据核的接缝测试（ticket #22）。
//!
//! 这里测的是**公共接缝** `farm_panel::core`：面板发出的每个请求、解出来的每个响应、
//! WS 上的每一帧。夹具 JSON 是手写的线格式（独立于实现的真相源），不是从类型反推的。
//!
//! 最重要的一条不变式：**面板只会说契约路由表里的路径**——ADR-0002 §1「面板是契约的
//! 另一个客户端，没有私有后门」由此可机械验证。

use farm_panel::core::{
    parse_ws_frame, ContractClient, Decoded, HttpRequest, PanelError, PanelOp, WsFrame,
    OP_TOKEN_HEADER,
};
use farm_sdk::services::agent_runner::{AttemptState, Disposition};
use farm_sdk::services::tracker::{IssueListResponse, IssueState, IssueView, ReleaseOutcome};
use farm_sdk::{ClientKind, ErrorCode, EventKind, CONTRACT_ROUTES, CONTRACT_VERSION, HANDSHAKE_PATH};

const TOKEN: &str = "op-token-abc123";

fn client() -> ContractClient {
    ContractClient::new("https://farm.example", TOKEN)
}

/// 面板可能发出的全部操作（工单点名的 HITL 操作面 + 三条读）。
fn every_op() -> Vec<PanelOp> {
    vec![
        PanelOp::Handshake,
        PanelOp::LoadIssues,
        PanelOp::LoadMappings,
        PanelOp::LoadEventHistory {
            since: Some("evt-7".into()),
            ticket_id: Some("22".into()),
            limit: Some(200),
        },
        PanelOp::OpenEventStream,
        PanelOp::Claim {
            issue_id: "22".into(),
            assignee: "panel".into(),
        },
        PanelOp::Release {
            issue_id: "22".into(),
            assignee: "panel".into(),
        },
        PanelOp::Steer {
            attempt_id: "att-22-1".into(),
            message: "keep the tests green".into(),
        },
        PanelOp::FollowUp {
            attempt_id: "att-22-1".into(),
            message: "then update the docs".into(),
        },
        PanelOp::Abort {
            attempt_id: "att-22-1".into(),
            reason: Some("human stopped it".into()),
        },
        PanelOp::ClearQueue {
            attempt_id: "att-22-1".into(),
        },
    ]
}

/// 请求打到的契约路由（WS 升级是路由本身，路径即契约路径）。
///
/// 路径参数路由（`/attempts/{attempt_id}/steer`）按段匹配：`{...}` 段是通配。
fn contract_route_of(request: &HttpRequest) -> Option<&'static str> {
    if request.path == HANDSHAKE_PATH || request.path == "/v1/events/stream" {
        return Some(Box::leak(request.path.clone().into_boxed_str()));
    }
    let segments: Vec<&str> = request.path.split('/').collect();
    CONTRACT_ROUTES
        .iter()
        .find(|route| {
            let template: Vec<&str> = route.path.split('/').collect();
            template.len() == segments.len()
                && template
                    .iter()
                    .zip(&segments)
                    .all(|(want, got)| want.starts_with('{') && want.ends_with('}') || want == got)
        })
        .map(|route| route.path)
}

#[test]
fn every_panel_op_targets_a_declared_contract_route() {
    // 无私有后门：面板的操作面是 CONTRACT_ROUTES 的子集（+ 握手路由）。
    let client = client();
    for op in every_op() {
        let request = client.request_for(&op);
        let route = contract_route_of(&request)
            .unwrap_or_else(|| panic!("面板发出了契约外的路径 {}（{:?}）", request.path, op));
        assert!(
            CONTRACT_ROUTES.iter().any(|r| r.path == route) || route == HANDSHAKE_PATH,
            "{route} 既不是契约路由也不是握手路由"
        );
    }
}

#[test]
fn every_panel_op_sends_the_same_op_token_the_orchestrator_sends() {
    // 单 token 同源：面板与编排器走同一个 header，不另立标准。
    let client = client();
    for op in every_op() {
        let request = client.request_for(&op);
        let header = request
            .headers
            .iter()
            .find(|(name, _)| name == OP_TOKEN_HEADER)
            .unwrap_or_else(|| panic!("{:?} 没带 op token", op));
        assert_eq!(header.1, TOKEN, "{:?} 的 op token 不对", op);
    }
}

#[test]
fn a_claim_request_carries_the_ticket_and_the_assignee() {
    let request = client().request_for(&PanelOp::Claim {
        issue_id: "22".into(),
        assignee: "panel".into(),
    });

    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/tracker/claims");
    assert_eq!(
        request.body.as_deref(),
        Some(r#"{"issue_id":"22","assignee":"panel"}"#)
    );
}

#[test]
fn a_release_request_names_the_claimant_handing_the_ticket_back() {
    let request = client().request_for(&PanelOp::Release {
        issue_id: "22".into(),
        assignee: "panel".into(),
    });

    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/tracker/releases");
    assert_eq!(
        request.body.as_deref(),
        Some(r#"{"issue_id":"22","assignee":"panel"}"#)
    );
}

#[test]
fn a_steer_request_puts_the_attempt_id_in_the_path_and_the_message_in_the_body() {
    let request = client().request_for(&PanelOp::Steer {
        attempt_id: "att-22-1".into(),
        message: "keep the tests green".into(),
    });

    assert_eq!(request.method, "POST");
    assert_eq!(
        request.path,
        "/v1/agent-runner/attempts/att-22-1/steer"
    );
    assert_eq!(
        request.body.as_deref(),
        Some(r#"{"message":"keep the tests green"}"#)
    );
}

#[test]
fn a_follow_up_request_is_the_same_shape_as_steer_on_its_own_route() {
    let steer = client().request_for(&PanelOp::Steer {
        attempt_id: "att-22-1".into(),
        message: "same words".into(),
    });
    let follow_up = client().request_for(&PanelOp::FollowUp {
        attempt_id: "att-22-1".into(),
        message: "same words".into(),
    });

    assert_eq!(follow_up.method, steer.method);
    assert_eq!(follow_up.body, steer.body, "受理形状同构");
    assert_eq!(
        follow_up.path,
        "/v1/agent-runner/attempts/att-22-1/follow-up"
    );
}

#[test]
fn an_abort_request_carries_the_reason_only_when_there_is_one() {
    let with_reason = client().request_for(&PanelOp::Abort {
        attempt_id: "att-22-1".into(),
        reason: Some("human stopped it".into()),
    });
    assert_eq!(with_reason.path, "/v1/agent-runner/attempts/att-22-1/abort");
    assert_eq!(
        with_reason.body.as_deref(),
        Some(r#"{"reason":"human stopped it"}"#)
    );

    let without = client().request_for(&PanelOp::Abort {
        attempt_id: "att-22-1".into(),
        reason: None,
    });
    assert_eq!(
        without.body.as_deref(),
        Some("{}"),
        "没给理由也要发一个合法 JSON 体"
    );
}

#[test]
fn a_clear_queue_request_has_no_parameters() {
    let request = client().request_for(&PanelOp::ClearQueue {
        attempt_id: "att-22-1".into(),
    });

    assert_eq!(request.method, "POST");
    assert_eq!(
        request.path,
        "/v1/agent-runner/attempts/att-22-1/clear-queue"
    );
    assert_eq!(request.body.as_deref(), Some("{}"));
}

#[test]
fn the_event_history_query_carries_the_snapshot_cursor_and_filters() {
    let request = client().request_for(&PanelOp::LoadEventHistory {
        since: Some("evt-7".into()),
        ticket_id: Some("22".into()),
        limit: Some(200),
    });

    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/v1/events/history");
    assert!(request.query.contains(&("since".to_string(), "evt-7".to_string())));
    assert!(request.query.contains(&("ticket_id".to_string(), "22".to_string())));
    assert!(request.query.contains(&("limit".to_string(), "200".to_string())));
    assert!(request.body.is_none(), "GET 不带体");
}

#[test]
fn the_reads_are_plain_gets_on_the_contract_routes() {
    let client = client();
    assert_eq!(client.request_for(&PanelOp::LoadIssues).path, "/v1/tracker/issues");
    assert_eq!(
        client.request_for(&PanelOp::LoadMappings).path,
        "/v1/workspace/mappings"
    );
    assert_eq!(
        client.request_for(&PanelOp::OpenEventStream).path,
        "/v1/events/stream"
    );
}

#[test]
fn the_ws_stream_url_is_the_contract_event_stream_route() {
    assert_eq!(
        client().stream_url(),
        "https://farm.example/v1/events/stream"
    );
}

#[test]
fn the_handshake_declares_the_panel_as_the_client_kind() {
    let handshake = client().handshake_request();

    assert_eq!(handshake.contract_version, CONTRACT_VERSION);
    assert_eq!(handshake.client.kind, ClientKind::Panel);
    assert!(!handshake.client.name.is_empty());
}

// ---------------------------------------------------------------------------
// 响应解码：夹具 JSON 是手写的线格式
// ---------------------------------------------------------------------------

/// `GET /v1/tracker/issues` 的真实响应形状（手写夹具）。
const TRACKER_VIEW_FIXTURE: &str = r#"{
  "issues": [
    {"issue_id":"22","title":"panel batch 1","state":"blocked","labels":["panel"],
     "assignee":"panel","updated_at":"2026-01-02T03:04:05Z","blocked_by":[],"blocking":[]},
    {"issue_id":"13","title":"tracker service","state":"in_progress","labels":["tracker"],
     "assignee":"orchestrator-a","updated_at":"2026-01-02T03:04:05Z","blocked_by":[],"blocking":[]}
  ]
}"#;

fn response(status: u16, body: &str) -> farm_panel::core::HttpResponse {
    farm_panel::core::HttpResponse {
        status,
        body: body.as_bytes().to_vec(),
    }
}

#[test]
fn the_tracker_view_fixture_decodes_into_the_board_snapshot() {
    let decoded = client()
        .decode(&PanelOp::LoadIssues, &response(200, TRACKER_VIEW_FIXTURE))
        .expect("tracker view must decode");

    let Decoded::Issues(issues) = decoded else {
        panic!("期待 IssueListResponse，实际 {decoded:?}");
    };
    let IssueListResponse { issues } = issues;
    assert_eq!(issues.len(), 2);
    let blocked: &IssueView = issues.iter().find(|i| i.issue_id == "22").expect("#22");
    assert_eq!(blocked.state, IssueState::Blocked);
    assert_eq!(blocked.assignee.as_deref(), Some("panel"));
}

#[test]
fn a_conflict_error_envelope_decodes_into_a_closed_set_error_naming_the_holder() {
    // 原子 claim 失败的线格式（409 + 闭集信封 + details.holder）。
    let body = r#"{"code":"conflict","message":"issue 22 is already claimed by orchestrator-a",
                 "details":{"issue_id":"22","holder":"orchestrator-a"},"retryable":false}"#;

    let err = client()
        .decode(
            &PanelOp::Claim {
                issue_id: "22".into(),
                assignee: "panel".into(),
            },
            &response(409, body),
        )
        .expect_err("未抢到必须报错");

    let PanelError::Contract(err) = err else {
        panic!("期待闭集错误信封，实际 {err:?}");
    };
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(err.http_status(), 409);
    assert_eq!(
        err.details.as_ref().and_then(|d| d.get("holder")),
        Some(&serde_json::json!("orchestrator-a")),
        "面板要把当前持有者显示出来"
    );
}

#[test]
fn a_successful_claim_decodes_into_the_winning_assignee() {
    let body = r#"{"issue_id":"22","claimed":true,"holder":"panel"}"#;

    let decoded = client()
        .decode(
            &PanelOp::Claim {
                issue_id: "22".into(),
                assignee: "panel".into(),
            },
            &response(200, body),
        )
        .expect("claim must decode");

    let Decoded::Claimed(claim) = decoded else {
        panic!("期待 ClaimResponse，实际 {decoded:?}");
    };
    assert!(claim.claimed);
    assert_eq!(claim.holder.as_deref(), Some("panel"));
}

#[test]
fn a_release_response_decodes_into_its_outcome() {
    let released = r#"{"issue_id":"22","outcome":"released"}"#;
    let idempotent = r#"{"issue_id":"22","outcome":"not_claimed"}"#;
    let client = client();
    let op = PanelOp::Release {
        issue_id: "22".into(),
        assignee: "panel".into(),
    };

    let Decoded::Released(first) = client.decode(&op, &response(200, released)).expect("decode") else {
        panic!("期待 ReleaseResponse")
    };
    assert_eq!(first.outcome, ReleaseOutcome::Released);
    let Decoded::Released(second) = client.decode(&op, &response(200, idempotent)).expect("decode")
    else {
        panic!("期待 ReleaseResponse")
    };
    assert_eq!(second.outcome, ReleaseOutcome::NotClaimed);
}

#[test]
fn a_steer_response_decodes_with_its_disposition_echo() {
    let body = r#"{"attempt_id":"att-22-1","state":"running","disposition":"queued"}"#;

    let decoded = client()
        .decode(
            &PanelOp::Steer {
                attempt_id: "att-22-1".into(),
                message: "keep the tests green".into(),
            },
            &response(200, body),
        )
        .expect("steer must decode");

    let Decoded::Steered(steer) = decoded else {
        panic!("期待 SteerResponse，实际 {decoded:?}");
    };
    assert_eq!(steer.attempt_id, "att-22-1");
    assert_eq!(steer.state, AttemptState::Running);
    let disposition: Disposition = steer.disposition.expect("disposition 回显");
    assert_eq!(disposition.as_str(), "queued");
    assert!(disposition.is_accepted());
}

#[test]
fn an_abort_response_decodes_into_the_accepted_state() {
    let body = r#"{"attempt_id":"att-22-1","state":"running","reason":"human stopped it"}"#;

    let decoded = client()
        .decode(
            &PanelOp::Abort {
                attempt_id: "att-22-1".into(),
                reason: Some("human stopped it".into()),
            },
            &response(200, body),
        )
        .expect("abort must decode");

    let Decoded::Aborted(abort) = decoded else {
        panic!("期待 AbortResponse，实际 {decoded:?}");
    };
    assert_eq!(abort.state, AttemptState::Running, "abort 只保证已受理");
    assert_eq!(abort.reason.as_deref(), Some("human stopped it"));
}

#[test]
fn a_clear_queue_response_decodes_into_the_number_of_dropped_injections() {
    let body = r#"{"attempt_id":"att-22-1","cleared":2}"#;

    let decoded = client()
        .decode(
            &PanelOp::ClearQueue {
                attempt_id: "att-22-1".into(),
            },
            &response(200, body),
        )
        .expect("clear-queue must decode");

    let Decoded::QueueCleared(cleared) = decoded else {
        panic!("期待 ClearQueueResponse，实际 {decoded:?}");
    };
    assert_eq!(cleared.cleared, 2);
}

#[test]
fn an_unauthorized_envelope_is_reported_as_unauthorized_not_as_a_parse_failure() {
    let body = r#"{"code":"unauthorized","message":"missing or invalid orchestrator op token","retryable":false}"#;

    let err = client()
        .decode(&PanelOp::LoadIssues, &response(401, body))
        .expect_err("401 必须报错");

    let PanelError::Contract(err) = err else {
        panic!("期待闭集错误信封，实际 {err:?}");
    };
    assert_eq!(err.code, ErrorCode::Unauthorized);
}

#[test]
fn a_malformed_body_is_a_client_error_and_not_a_panic() {
    let err = client()
        .decode(&PanelOp::LoadIssues, &response(200, "{\"issues\": ["))
        .expect_err("坏 JSON 必须报错");

    assert!(
        matches!(err, PanelError::MalformedResponse { .. }),
        "实际 {err:?}"
    );
}

#[test]
fn a_success_status_with_an_error_envelope_body_is_still_a_contract_error() {
    // 宿主不会这么回，但客户端不能把错误信封当成成功投影。
    let body = r#"{"code":"internal","message":"boom","retryable":false}"#;

    let err = client()
        .decode(&PanelOp::LoadIssues, &response(200, body))
        .expect_err("错误信封不是 IssueListResponse");

    assert!(matches!(err, PanelError::MalformedResponse { .. }), "{err:?}");
}

// ---------------------------------------------------------------------------
// WS 帧：hello / 事件 / 错误信封同一通道
// ---------------------------------------------------------------------------

#[test]
fn the_hello_frame_is_told_apart_from_an_event_frame_and_an_error_frame() {
    let hello = parse_ws_frame(
        r#"{"type":"hello","contract_version":"1.0.0","event_kinds":["blocked","needs_attention"]}"#,
    )
    .expect("hello frame");
    let WsFrame::Hello(hello) = hello else {
        panic!("期待 WsHello，实际 {hello:?}");
    };
    assert_eq!(hello.contract_version, CONTRACT_VERSION);
    assert!(hello.event_kinds.contains(&EventKind::Blocked));
    assert!(hello.event_kinds.contains(&EventKind::NeedsAttention));

    let error = parse_ws_frame(r#"{"code":"service_unavailable","message":"store down","retryable":true}"#)
        .expect("error frame");
    let WsFrame::Error(err) = error else {
        panic!("期待闭集错误帧，实际 {error:?}");
    };
    assert_eq!(err.code, ErrorCode::ServiceUnavailable);
    assert!(err.retryable);
}

#[test]
fn an_event_frame_decodes_into_the_envelope_the_board_reduces() {
    let frame = parse_ws_frame(
        r#"{"id":"evt-9","kind":"needs_attention","occurred_at":"2026-01-02T03:04:05Z",
            "actor":"agent-runner","ticket_id":"22","attempt_id":"att-22-1",
            "payload":{"reason":"credential courier failed"},"audited":false}"#,
    )
    .expect("event frame");

    let WsFrame::Event(envelope) = frame else {
        panic!("期待 EventEnvelope，实际 {frame:?}");
    };
    assert_eq!(envelope.id, "evt-9");
    assert_eq!(envelope.kind, EventKind::NeedsAttention);
    assert_eq!(envelope.ticket_id.as_deref(), Some("22"));
    assert_eq!(envelope.attempt_id.as_deref(), Some("att-22-1"));
    assert_eq!(envelope.payload["reason"], "credential courier failed");
}

#[test]
fn an_unparsable_frame_is_a_client_error_and_not_a_panic() {
    let err = parse_ws_frame("not json at all").expect_err("坏帧必须报错");
    assert!(matches!(err, PanelError::MalformedFrame { .. }), "{err:?}");
}
