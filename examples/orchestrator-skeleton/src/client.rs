//! 契约客户端 + 冒烟：握手 + 闭集错误演示。
//!
//! 用一个极简 HTTP/1.1 客户端（`Connection: close` + 读到 EOF）证明契约可达，
//! 不引 reqwest——骨架的立场是「用最少机器证明契约面成立」。

use std::net::SocketAddr;

use farm_sdk::handshake::HandshakeResponse;
use farm_sdk::ContractError;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 一次冒烟检查的结果。
#[derive(Clone, Debug)]
pub struct SmokeCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// 极简 HTTP/1.1 请求：返回 `(状态码, body)`。依赖 `Connection: close` 读到 EOF。
async fn http_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(addr).await?;
    let body = body.unwrap_or("");
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if let Some(t) = token {
        req.push_str(&format!("X-Farm-Token: {t}\r\n"));
    }
    if !body.is_empty() {
        req.push_str("Content-Type: application/json\r\n");
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let payload = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    Ok((status, payload))
}

/// 从错误体里取闭集 `code` 字段。
fn error_code(body: &str) -> Option<String> {
    serde_json::from_str::<ContractError>(body)
        .ok()
        .map(|e| e.code.as_str().to_string())
        .or_else(|| {
            serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_string))
        })
}

/// 跑一遍契约冒烟：握手 + 四类闭集错误。逐项记录，不中途失败。
pub async fn run_contract_smoke(addr: SocketAddr, token: &str) -> Vec<SmokeCheck> {
    let mut checks = Vec::new();

    // 1) 兼容版本的握手 → 200 + HandshakeResponse。
    let hs_body = format!(
        r#"{{"contract_version":"1.0.0","client":{{"name":"orchestrator-skeleton","version":"0.1.0","kind":"orchestrator"}}}}"#
    );
    match http_request(addr, "POST", "/handshake", Some(token), Some(&hs_body)).await {
        Ok((200, body)) => match serde_json::from_str::<HandshakeResponse>(&body) {
            Ok(hs) => checks.push(SmokeCheck {
                name: "handshake completes with a compatible contract version",
                ok: hs.contract_version.starts_with('1'),
                detail: format!("host {} offers {} services", hs.host_version, hs.services.len()),
            }),
            Err(e) => checks.push(SmokeCheck {
                name: "handshake completes with a compatible contract version",
                ok: false,
                detail: format!("unparseable HandshakeResponse: {e}: {body}"),
            }),
        },
        Ok((status, body)) => checks.push(SmokeCheck {
            name: "handshake completes with a compatible contract version",
            ok: false,
            detail: format!("expected 200, got {status}: {body}"),
        }),
        Err(e) => checks.push(SmokeCheck {
            name: "handshake completes with a compatible contract version",
            ok: false,
            detail: format!("connect failed: {e}"),
        }),
    }

    // 2) 缺 token → 401 unauthorized（与错 token 同码，避免预言机）。
    let no_token = http_request(addr, "GET", "/v1/tracker/issues", None, None).await;
    checks.push(classify(
        "a request without a token is a closed-set unauthorized",
        no_token,
        401,
        "unauthorized",
    ));

    // 3) 错 token → 401 unauthorized（同一码）。
    let bad_token = http_request(addr, "GET", "/v1/tracker/issues", Some("wrong-token"), None).await;
    checks.push(classify(
        "a request with a wrong token is the same closed-set unauthorized",
        bad_token,
        401,
        "unauthorized",
    ));

    // 4) 正确 token 打占位路由 → 501 not_implemented（骨架统一回答）。
    let placeholder = http_request(addr, "GET", "/v1/tracker/issues", Some(token), None).await;
    checks.push(classify(
        "an authed placeholder route answers a closed-set not_implemented",
        placeholder,
        501,
        "not_implemented",
    ));

    // 5) 不兼容契约版本 → contract_version_unsupported。
    let bad_ver = format!(
        r#"{{"contract_version":"2.0.0","client":{{"name":"orchestrator-skeleton","version":"0.1.0","kind":"orchestrator"}}}}"#
    );
    let incompatible = http_request(addr, "POST", "/handshake", Some(token), Some(&bad_ver)).await;
    checks.push(classify(
        "an incompatible contract version is a closed-set contract_version_unsupported",
        incompatible,
        400,
        "contract_version_unsupported",
    ));

    checks
}

/// 把一次请求结果归类成一条 SmokeCheck。
fn classify(
    name: &'static str,
    result: std::io::Result<(u16, String)>,
    want_status: u16,
    want_code: &str,
) -> SmokeCheck {
    match result {
        Ok((status, body)) => {
            let code = error_code(&body);
            let ok = status == want_status && code.as_deref() == Some(want_code);
            SmokeCheck {
                name,
                ok,
                detail: format!("status={status} code={code:?} (want {want_status}/{want_code})"),
            }
        }
        Err(e) => SmokeCheck {
            name,
            ok: false,
            detail: format!("request failed: {e}"),
        },
    }
}
