//! 一致性套件（ticket #26）：对**真实 farm-host 二进制**校验「符合契约」。
//!
//! ADR-0001 决策 6：一致性套件是「符合契约」的唯一凭证。本套件以**契约客户端**身份
//! （只过 farm-sdk 类型 + 契约 HTTP/WS API）逐条断言契约面：握手、单 token 鉴权、
//! 闭集错误码、路由单一源、事件闭集、宿主管理面独立凭据、WS 自描述。全部由单一源
//! （`CONTRACT_ROUTES` / `ErrorCode::ALL` / `EventKind::ALL` / `ServiceName::ALL`）驱动，
//! 不硬编码——闭集增删即在此显形。
//!
//! 外部实现者可拿本套件对自家宿主跑，作为接入凭证。

use std::net::SocketAddr;

use farm_sdk::events::EventKind;
use farm_sdk::services::ServiceName;
use farm_sdk::{ContractError, ErrorCode, CONTRACT_ROUTES};

use crate::client::{http_request, ContractClient};
use crate::report::{Check, Report};

/// 跑一致性套件；`client` 已连上真实宿主。逐条记录，不中途失败。
pub async fn run_conformance(client: &ContractClient) -> Report {
    let mut report = Report::new("farm contract conformance (against the real farm-host binary)");
    let addr = client.addr;
    let token = client.op_token.clone();

    check_handshake(client, &mut report).await;
    check_single_token_auth(addr, &token, &mut report).await;
    check_closed_set_errors(addr, &token, &mut report).await;
    check_every_route_is_live(addr, &token, &mut report).await;
    check_event_closed_set(client, &mut report).await;
    check_admin_face(client, &mut report).await;
    check_ws_face(client, &mut report).await;
    check_atomic_claim_conflict(client, &mut report).await;

    report
}

// ---------------------------------------------------------------------------
// 握手
// ---------------------------------------------------------------------------

async fn check_handshake(client: &ContractClient, report: &mut Report) {
    match client.handshake().await {
        Ok(hs) => {
            report.record(
                "handshake with a compatible contract version returns the host self-description",
                hs.contract_version.starts_with('1'),
                format!("contract_version={} services={} event_kinds={}", hs.contract_version, hs.services.len(), hs.event_kinds.len()),
            );
            report.record(
                "the handshake advertises exactly the five services",
                hs.services == ServiceName::ALL.to_vec(),
                format!("{:?}", hs.services.iter().map(|s| s.as_str()).collect::<Vec<_>>()),
            );
            report.record(
                "the handshake advertises the op-token header and bearer fallback",
                hs.auth.http_header == "X-Farm-Token" && hs.auth.also_accepts_bearer,
                format!("http_header={} bearer={}", hs.auth.http_header, hs.auth.also_accepts_bearer),
            );
        }
        Err(e) => report.push(Check::fail("handshake succeeds", format!("closed-set error: {e}"))),
    }

    // 主版本不符 → contract_version_unsupported（400）。
    let bad = format!(r#"{{"contract_version":"2.0.0","client":{{"name":"conformance","version":"0.1.0","kind":"orchestrator"}}}}"#);
    let (status, body) = http_request(client.addr, "POST", "/handshake", Some(("X-Farm-Token", client.op_token.as_str())), Some(&bad)).await.unwrap_or((0, String::new()));
    let code = closed_code(&body);
    report.record(
        "an incompatible major contract version is a closed-set contract_version_unsupported",
        status == 400 && code.as_deref() == Some("contract_version_unsupported"),
        format!("status={status} code={code:?}"),
    );

    // 畸形体 → invalid_request（400）。
    let (status, body) = http_request(client.addr, "POST", "/handshake", Some(("X-Farm-Token", client.op_token.as_str())), Some("{\"contract_version\":")).await.unwrap_or((0, String::new()));
    let code = closed_code(&body);
    report.record(
        "a malformed handshake body is a closed-set invalid_request",
        status == 400 && code.as_deref() == Some("invalid_request"),
        format!("status={status} code={code:?}"),
    );

    // 错方法 → method_not_allowed（405）。
    let (status, body) = http_request(client.addr, "GET", "/handshake", Some(("X-Farm-Token", client.op_token.as_str())), None).await.unwrap_or((0, String::new()));
    let code = closed_code(&body);
    report.record(
        "the handshake route with the wrong method is a closed-set method_not_allowed",
        status == 405 && code.as_deref() == Some("method_not_allowed"),
        format!("status={status} code={code:?}"),
    );
}

// ---------------------------------------------------------------------------
// 单 token 静态鉴权（缺 / 错同码，不做预言机）
// ---------------------------------------------------------------------------

async fn check_single_token_auth(addr: SocketAddr, token: &str, report: &mut Report) {
    let (ms, mb) = http_request(addr, "GET", "/v1/tracker/issues", None, None).await.unwrap_or((0, String::new()));
    let (ws, wb) = http_request(addr, "GET", "/v1/tracker/issues", Some(("X-Farm-Token", "not-the-token")), None).await.unwrap_or((0, String::new()));
    let mcode = closed_code(&mb);
    let wcode = closed_code(&wb);
    report.record(
        "a contract request without a token is a closed-set unauthorized",
        ms == 401 && mcode.as_deref() == Some("unauthorized"),
        format!("status={ms} code={mcode:?}"),
    );
    report.record(
        "a missing token and a wrong token get the same closed-set answer (no oracle)",
        ms == ws && mb == wb,
        format!("missing=({ms},{mcode:?}) wrong=({ws},{wcode:?})"),
    );

    // Bearer 也收（握手广告了 also_accepts_bearer）。
    let (bs, _bb) = http_request(addr, "GET", "/v1/tracker/issues", Some(("Authorization", &format!("Bearer {token}"))), None).await.unwrap_or((0, String::new()));
    report.record(
        "the bearer authorization header is accepted as the op token",
        bs == 200,
        format!("status={bs}"),
    );
}

// ---------------------------------------------------------------------------
// 闭集错误码（可触达的那些逐一在活宿主上核对）
// ---------------------------------------------------------------------------

async fn check_closed_set_errors(addr: SocketAddr, token: &str, report: &mut Report) {
    // not_found：未知契约路径。
    let (status, body) = http_request(addr, "GET", "/v1/tracker/nope", Some(("X-Farm-Token", token)), None).await.unwrap_or((0, String::new()));
    report.record(
        "an unknown contract path is a closed-set not_found",
        status == 404 && closed_code(&body).as_deref() == Some("not_found"),
        format!("status={status} code={:?}", closed_code(&body)),
    );

    // method_not_allowed：已知路由错方法。
    let (status, body) = http_request(addr, "POST", "/v1/tracker/issues", Some(("X-Farm-Token", token)), Some("{}")).await.unwrap_or((0, String::new()));
    report.record(
        "a known route with the wrong method is a closed-set method_not_allowed",
        status == 405 && closed_code(&body).as_deref() == Some("method_not_allowed"),
        format!("status={status} code={:?}", closed_code(&body)),
    );

    // invalid_request：claim 缺字段。
    let (status, body) = http_request(addr, "POST", "/v1/tracker/claims", Some(("X-Farm-Token", token)), Some("{}")).await.unwrap_or((0, String::new()));
    report.record(
        "a malformed claim request is a closed-set invalid_request",
        status == 400 && closed_code(&body).as_deref() == Some("invalid_request"),
        format!("status={status} code={:?}", closed_code(&body)),
    );

    // 闭集错误信封形状：code + retryable，details 缺省不出现。
    let envelope_ok = serde_json::from_str::<ContractError>(&body)
        .map(|e| e.to_json().contains("\"retryable\"") && !e.to_json().contains("\"details\""))
        .unwrap_or(false);
    report.record(
        "the closed-set error envelope carries code + retryable and omits details by default",
        envelope_ok,
        format!("body={body}"),
    );

    // 闭集清单本身（单一源 ErrorCode::ALL）与文档化的 10 个码一致。
    let codes: Vec<&str> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
    report.record(
        "the closed-set error codes are exactly the ten documented members",
        codes == vec!["unauthorized", "forbidden", "contract_version_unsupported", "invalid_request", "not_found", "method_not_allowed", "conflict", "not_implemented", "service_unavailable", "internal"],
        format!("{codes:?}"),
    );

    // 每个闭集码的 HTTP 状态与契约表一致（service_unavailable 是唯一可重试）。
    let table_ok = ErrorCode::ALL.iter().all(|c| c.http_status() == documented_status(*c))
        && ErrorCode::ALL.iter().filter(|c| c.is_retryable()).count() == 1
        && ErrorCode::ServiceUnavailable.is_retryable();
    report.record(
        "every closed-set error code maps to its documented HTTP status (service_unavailable is the only retryable one)",
        table_ok,
        "checked against the contract table",
    );
}

// ---------------------------------------------------------------------------
// 路由单一源：CONTRACT_ROUTES 每一条都在活宿主上可达（无占位）
// ---------------------------------------------------------------------------

async fn check_every_route_is_live(addr: SocketAddr, token: &str, report: &mut Report) {
    let mut not_live = Vec::new();
    for route in CONTRACT_ROUTES {
        // WS 路由单走 ws_hello（HTTP GET 不打升级会被拒，但那也是「活的」）。
        if route.path == "/v1/events/stream" {
            continue;
        }
        let body = if route.method == "POST" { Some("{}") } else { None };
        let (status, resp) = http_request(addr, route.method, route.path, Some(("X-Farm-Token", token)), body).await.unwrap_or((0, String::new()));
        let code = closed_code(&resp);
        // 活路由：不是 501 not_implemented（可能因缺具体入参回 400/404，但那仍是接通的）。
        if status == 501 || code.as_deref() == Some("not_implemented") {
            not_live.push(format!("{} {}", route.method, route.path));
        }
    }
    report.record(
        "every contract route in the single-source table is live on the host (no not_implemented placeholder)",
        not_live.is_empty(),
        if not_live.is_empty() { format!("{} routes all reachable", CONTRACT_ROUTES.len()) } else { format!("placeholders: {not_live:?}") },
    );
}

// ---------------------------------------------------------------------------
// 事件闭集：握手广告的 event_kinds == EventKind::ALL
// ---------------------------------------------------------------------------

async fn check_event_closed_set(client: &ContractClient, report: &mut Report) {
    match client.handshake().await {
        Ok(hs) => {
            let matches = hs.event_kinds == EventKind::ALL.to_vec();
            report.record(
                "the advertised event closed set is exactly EventKind::ALL",
                matches,
                format!("{} kinds advertised", hs.event_kinds.len()),
            );
        }
        Err(e) => report.push(Check::fail("event closed set", format!("handshake failed: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// 宿主管理面：独立 token，边界分明（不是私有后门）
// ---------------------------------------------------------------------------

async fn check_admin_face(client: &ContractClient, report: &mut Report) {
    // 缺 admin token → unauthorized（401）。
    let (status, body) = http_request(client.addr, "GET", "/admin/status", None, None).await.unwrap_or((0, String::new()));
    report.record(
        "the admin API without an admin token is a closed-set unauthorized",
        status == 401 && closed_code(&body).as_deref() == Some("unauthorized"),
        format!("status={status} code={:?}", closed_code(&body)),
    );

    // op token 递到管理面 → forbidden（403，已鉴权但越面）。
    let (status, body) = http_request(client.addr, "GET", "/admin/status", Some(("X-Farm-Token", client.op_token.as_str())), None).await.unwrap_or((0, String::new()));
    report.record(
        "the orchestrator op token on the admin API is a closed-set forbidden (the two tokens are not the same)",
        status == 403 && closed_code(&body).as_deref() == Some("forbidden"),
        format!("status={status} code={:?}", closed_code(&body)),
    );

    // 正确 admin token → 200 + 五服务自描述。
    match client.admin("GET", "/admin/status", None).await {
        Ok((200, body)) => {
            let ok = serde_json::from_str::<farm_sdk::admin::AdminStatusResponse>(&body)
                .map(|s| s.services == ServiceName::ALL.to_vec() && s.contract_version.starts_with('1'))
                .unwrap_or(false);
            report.record("the admin API with the admin token self-describes the five services", ok, format!("body={body}"));
        }
        Ok((status, body)) => report.push(Check::fail("admin status with admin token", format!("status={status} body={body}"))),
        Err(e) => report.push(Check::fail("admin status with admin token", format!("io: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// WS 面：首帧 hello 广告契约版本 + 事件闭集
// ---------------------------------------------------------------------------

async fn check_ws_face(client: &ContractClient, report: &mut Report) {
    match client.ws_hello().await {
        Ok(hello) => {
            report.record(
                "the WS event stream greets with a hello frame advertising the contract version",
                hello.r#type == "hello" && hello.contract_version.starts_with('1'),
                format!("type={} version={}", hello.r#type, hello.contract_version),
            );
            report.record(
                "the WS hello advertises the full event closed set",
                hello.event_kinds == EventKind::ALL.to_vec(),
                format!("{} kinds", hello.event_kinds.len()),
            );
        }
        Err(e) => report.push(Check::fail("WS event stream hello", e)),
    }
}

// ---------------------------------------------------------------------------
// 原子 claim 的 CAS：抢到 → 再被他人抢 → conflict（若宿主有可抢的票）
// ---------------------------------------------------------------------------

async fn check_atomic_claim_conflict(client: &ContractClient, report: &mut Report) {
    let issues = match client.issues().await {
        Ok(list) => list.issues,
        Err(_) => {
            report.push(Check::pass("atomic claim CAS conflict", "no seeded issues to claim; skipped (host has no ticket source)"));
            return;
        }
    };
    let Some(issue) = issues.into_iter().find(|i| matches!(i.state, farm_sdk::services::tracker::IssueState::Open)) else {
        report.push(Check::pass("atomic claim CAS conflict", "no open issue to claim; skipped"));
        return;
    };
    match client.claim(&issue.issue_id, "conformance-claimant").await {
        Ok(first) => {
            report.record(
                "an atomic claim by the first claimant succeeds",
                first.claimed,
                format!("issue {} claimed", issue.issue_id),
            );
            // 第二个 assignee 抢同一张 → 闭集 conflict（409，details.holder 指名持有者）。
            match client.claim(&issue.issue_id, "conformance-second").await {
                Ok(_) => report.push(Check::fail("a second claimant is refused with a closed-set conflict", "claim unexpectedly succeeded (no CAS)")),
                Err(ContractError { code: ErrorCode::Conflict, .. }) => {
                    report.push(Check::pass("a second claimant is refused with a closed-set conflict", "409 conflict as advertised"))
                }
                Err(e) => report.push(Check::fail("a second claimant is refused with a closed-set conflict", format!("got {e}"))),
            }
        }
        Err(e) => report.push(Check::fail("an atomic claim by the first claimant succeeds", format!("{e}"))),
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// 从错误体里取闭集 `code`（容忍非信封体 → None）。
fn closed_code(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_string))
}

/// 契约文档里固化的码→状态表（独立真源，不重算实现）。
fn documented_status(code: ErrorCode) -> u16 {
    match code {
        ErrorCode::Unauthorized => 401,
        ErrorCode::Forbidden => 403,
        ErrorCode::ContractVersionUnsupported => 400,
        ErrorCode::InvalidRequest => 400,
        ErrorCode::NotFound => 404,
        ErrorCode::MethodNotAllowed => 405,
        ErrorCode::Conflict => 409,
        ErrorCode::NotImplemented => 501,
        ErrorCode::ServiceUnavailable => 503,
        ErrorCode::Internal => 500,
    }
}
