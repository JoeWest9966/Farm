//! 契约客户端 + 冒烟：握手 + 闭集错误演示。
//!
//! 用一个极简 HTTP/1.1 客户端（`Connection: close` + 读到 EOF）证明契约可达，
//! 不引 reqwest——骨架的立场是「用最少机器证明契约面成立」。WS 事件流另用
//! tokio-tungstenite（真实编排器订阅事件面的正当依赖，workspace 已在用）。
//!
//! [`ContractClient`] 是加固版骨架的核心：以**契约客户端**身份（只过 farm-sdk 类型 +
//! 契约 HTTP/WS API，无私有后门）驱动整链与一致性套件。每一条 typed 方法都对应一条
//! 契约路由，错误一律解析成闭集 [`ContractError`]。

use std::net::SocketAddr;

use farm_sdk::handshake::HandshakeResponse;
use farm_sdk::services::agent_runner::{DispatchRequest, DispatchResponse};
use farm_sdk::services::events::EventHistoryResponse;
use farm_sdk::services::merge::{MergeProposal, MergeProposalResponse};
use farm_sdk::services::tracker::{ClaimRequest, ClaimResponse, IssueListResponse};
use farm_sdk::services::workspace::{
    MappingListResponse, WorktreeEstablishRequest, WorktreeRef,
};
use farm_sdk::notifier::{NotificationChannelConfig, NotificationChannelUpdate};
use farm_sdk::workflow::WorkflowConfigStatus;
use farm_sdk::{AuthScheme, ContractError};
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
///
/// `auth` 是可选的一对 `(header 名, 值)`（契约面 `X-Farm-Token` / 管理面
/// `X-Farm-Admin-Token`）——两个面凭据不同，故 header 名也作为参数，不写死。
pub async fn http_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    auth: Option<(&str, &str)>,
    body: Option<&str>,
) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(addr).await?;
    let body = body.unwrap_or("");
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if let Some((header, value)) = auth {
        req.push_str(&format!("{header}: {value}\r\n"));
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
    match http_request(addr, "POST", "/handshake", Some(("X-Farm-Token", token)), Some(&hs_body)).await {
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
    let bad_token = http_request(addr, "GET", "/v1/tracker/issues", Some(("X-Farm-Token", "wrong-token")), None).await;
    checks.push(classify(
        "a request with a wrong token is the same closed-set unauthorized",
        bad_token,
        401,
        "unauthorized",
    ));

    // 4) 未知契约路径 → 404 not_found（闭集错误；契约面已无占位路由，全部服务打通）。
    let unknown = http_request(addr, "GET", "/v1/tracker/does-not-exist", Some(("X-Farm-Token", token)), None).await;
    checks.push(classify(
        "an unknown contract path is a closed-set not_found",
        unknown,
        404,
        "not_found",
    ));

    // 5) 不兼容契约版本 → contract_version_unsupported。
    let bad_ver = format!(
        r#"{{"contract_version":"2.0.0","client":{{"name":"orchestrator-skeleton","version":"0.1.0","kind":"orchestrator"}}}}"#
    );
    let incompatible = http_request(addr, "POST", "/handshake", Some(("X-Farm-Token", token)), Some(&bad_ver)).await;
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

// ---------------------------------------------------------------------------
// ContractClient：加固版契约客户端（只过 farm-sdk 类型 + 契约 HTTP/WS API）
// ---------------------------------------------------------------------------

/// 以契约客户端身份驱动宿主：契约面用 op token，宿主管理面用独立 admin token
/// （ADR-0002 §1：管理 token ≠ 操作 token）。所有方法把响应解析成 farm-sdk 类型，
/// 错误一律落闭集 [`ContractError`]——编排器因此能穷举失败形状。
#[derive(Clone, Debug)]
pub struct ContractClient {
    pub addr: SocketAddr,
    pub op_token: String,
    pub admin_token: Option<String>,
}

impl ContractClient {
    pub fn new(addr: SocketAddr, op_token: impl Into<String>, admin_token: Option<String>) -> Self {
        Self { addr, op_token: op_token.into(), admin_token }
    }

    fn op_auth(&self) -> (&'static str, &str) {
        // AuthScheme 是 header 名的单一源（与宿主广告的同一个常量）。
        let name: &'static str = "X-Farm-Token";
        let _ = AuthScheme::default(); // 保持单一源意图可见；常量即其值
        (name, self.op_token.as_str())
    }

    /// 契约面请求（op token）。
    pub async fn contract(&self, method: &str, path: &str, body: Option<&str>) -> std::io::Result<(u16, String)> {
        let (header, value) = self.op_auth();
        http_request(self.addr, method, path, Some((header, value)), body).await
    }

    /// 宿主管理面请求（admin token）。没配 admin token 就不发（调用方会拿到连接错误）。
    pub async fn admin(&self, method: &str, path: &str, body: Option<&str>) -> std::io::Result<(u16, String)> {
        const ADMIN_HEADER: &str = "X-Farm-Admin-Token";
        let _ = AuthScheme::default();
        match &self.admin_token {
            Some(token) => http_request(self.addr, method, path, Some((ADMIN_HEADER, token.as_str())), body).await,
            None => http_request(self.addr, method, path, None, body).await,
        }
    }

    /// 把一次契约请求的结果解析成 `T`（2xx）或闭集 [`ContractError`]（其余）。
    fn parse<T: serde::de::DeserializeOwned>(status: u16, body: &str) -> Result<T, ContractError> {
        if (200..300).contains(&status) {
            serde_json::from_str(body).map_err(|e| {
                ContractError::internal(format!("host returned HTTP {status} but an unparseable body: {e}: {body}"))
            })
        } else {
            // 宿主错误体就是闭集信封；解析不了也落成一个 internal，绝不静默成功。
            Err(serde_json::from_str::<ContractError>(body).unwrap_or_else(|e| {
                ContractError::internal(format!("host returned HTTP {status} with a non-envelope body: {e}: {body}"))
            }))
        }
    }

    // --- 握手 / 归一化视图 ---

    pub async fn handshake(&self) -> Result<HandshakeResponse, ContractError> {
        let body = r#"{"contract_version":"1.0.0","client":{"name":"orchestrator-skeleton","version":"0.1.0","kind":"orchestrator"}}"#;
        let (status, body) = self.contract("POST", "/handshake", Some(body)).await.map_err(io_err)?;
        Self::parse(status, &body)
    }

    pub async fn issues(&self) -> Result<IssueListResponse, ContractError> {
        let (status, body) = self.contract("GET", "/v1/tracker/issues", None).await.map_err(io_err)?;
        Self::parse(status, &body)
    }

    pub async fn claim(&self, issue_id: &str, assignee: &str) -> Result<ClaimResponse, ContractError> {
        let body = serde_json::to_string(&ClaimRequest { issue_id: issue_id.into(), assignee: assignee.into() }).expect("serialize");
        let (status, body) = self.contract("POST", "/v1/tracker/claims", Some(&body)).await.map_err(io_err)?;
        Self::parse(status, &body)
    }

    // --- Workspace 服务 ---

    pub async fn establish_worktree(&self, req: &WorktreeEstablishRequest) -> Result<WorktreeRef, ContractError> {
        let body = serde_json::to_string(req).expect("serialize");
        let (status, body) = self.contract("POST", "/v1/workspace/worktrees", Some(&body)).await.map_err(io_err)?;
        Self::parse(status, &body)
    }

    pub async fn mappings(&self) -> Result<MappingListResponse, ContractError> {
        let (status, body) = self.contract("GET", "/v1/workspace/mappings", None).await.map_err(io_err)?;
        Self::parse(status, &body)
    }

    // --- Agent-Runner ---

    pub async fn dispatch(&self, issue_id: &str, prompt: &str) -> Result<DispatchResponse, ContractError> {
        let body = serde_json::to_string(&DispatchRequest { issue_id: issue_id.into(), profile: None, prompt: Some(prompt.into()) }).expect("serialize");
        let (status, body) = self.contract("POST", "/v1/agent-runner/dispatches", Some(&body)).await.map_err(io_err)?;
        Self::parse(status, &body)
    }

    // --- Merge 服务 ---

    pub async fn propose_merge(&self, proposal: &MergeProposal) -> Result<MergeProposalResponse, ContractError> {
        let body = serde_json::to_string(proposal).expect("serialize");
        let (status, body) = self.contract("POST", "/v1/merge/proposals", Some(&body)).await.map_err(io_err)?;
        Self::parse(status, &body)
    }

    // --- 事件总线 ---

    pub async fn event_history(&self, limit: u32) -> Result<EventHistoryResponse, ContractError> {
        let path = format!("/v1/events/history?limit={limit}");
        let (status, body) = self.contract("GET", &path, None).await.map_err(io_err)?;
        Self::parse(status, &body)
    }

    // --- 宿主管理面（admin token）---

    pub async fn workflow_config(&self, repo_id: &str) -> Result<WorkflowConfigStatus, ContractError> {
        let path = format!("/admin/workflow/config/{repo_id}");
        let (status, body) = self.admin("GET", &path, None).await.map_err(io_err)?;
        Self::parse(status, &body)
    }

    pub async fn set_channel(&self, repo_id: &str, update: &NotificationChannelUpdate) -> Result<NotificationChannelConfig, ContractError> {
        let path = format!("/admin/notifier/channel/{repo_id}");
        let body = serde_json::to_string(update).expect("serialize");
        let (status, body) = self.admin("POST", &path, Some(&body)).await.map_err(io_err)?;
        Self::parse(status, &body)
    }

    // --- WS 事件面：连接、读首帧 hello（契约版本的活的证明）---

    /// 订阅 WS 事件流并读回首帧 `hello`（自描述契约版本 + 事件闭集）。
    ///
    /// 用最少机器：手写 WS upgrade（带 op token 头）+ 解析首个**服务端文本帧**
    /// （服务端帧不带掩码）。hello 是订阅建立后宿主立即回的自描述。
    pub async fn ws_hello(&self) -> Result<farm_sdk::events::WsHello, String> {
        use tokio::io::AsyncReadExt;
        let mut stream = TcpStream::connect(self.addr).await.map_err(|e| format!("connect: {e}"))?;
        let upgrade = format!(
            "GET /v1/events/stream HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nX-Farm-Token: {}\r\n\r\n",
            self.addr, self.op_token
        );
        stream.write_all(upgrade.as_bytes()).await.map_err(|e| format!("write upgrade: {e}"))?;
        stream.flush().await.map_err(|e| format!("flush: {e}"))?;

        // 读响应头（到空行为止）；确认 101（token 已在中间件验过，缺/错会先 401）。
        let mut header = Vec::new();
        let mut byte = [0u8; 1];
        while !header.ends_with(b"\r\n\r\n") {
            let n = stream.read(&mut byte).await.map_err(|e| format!("read header: {e}"))?;
            if n == 0 {
                return Err(format!("connection closed during ws upgrade: {}", String::from_utf8_lossy(&header)));
            }
            header.extend_from_slice(&byte[..n]);
        }
        let status_line = String::from_utf8_lossy(&header);
        let status_line = status_line.lines().next().unwrap_or_default().to_string();
        if !status_line.contains("101") {
            return Err(format!("ws upgrade was not accepted: {status_line}"));
        }

        // 读首个服务端文本帧（不带掩码）：2 字节头 + 扩展长度 + payload。
        let payload = read_ws_frame(&mut stream).await.map_err(|e| format!("read hello frame: {e}"))?;
        serde_json::from_str(&payload).map_err(|e| format!("hello frame is not a WsHello: {e}: {payload}"))
    }
}

/// IO 错误 → 闭集 `service_unavailable`（宿主不可达；可重试）。
fn io_err(e: std::io::Error) -> ContractError {
    ContractError::service_unavailable(format!("host unreachable: {e}"))
}

/// 读一个**服务端** WS 数据帧（服务端→客户端帧不带掩码），返回文本 payload。
///
/// 只承接 hello 帧所需：FIN+text  opcode、7/16/64 位三档长度。控制帧（ping/pong/close）
/// 不在首帧契约里（axum 自动应答 ping），故不特殊处理。
async fn read_ws_frame(stream: &mut tokio::net::TcpStream) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut header = [0u8; 2];
    stream.read_exact(&mut header).await?;
    let opcode = header[0] & 0x0F;
    if opcode != 0x1 {
        return Err(std::io::Error::other(format!(
            "expected a text frame (opcode 0x1), got opcode {opcode}"
        )));
    }
    let len7 = header[1] & 0x7F;
    let len: u64 = match len7 {
        126 => {
            let mut ext = [0u8; 2];
            stream.read_exact(&mut ext).await?;
            u16::from_be_bytes(ext) as u64
        }
        127 => {
            let mut ext = [0u8; 8];
            stream.read_exact(&mut ext).await?;
            u64::from_be_bytes(ext)
        }
        n => n as u64,
    };
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).await?;
    Ok(String::from_utf8_lossy(&payload).into_owned())
}
