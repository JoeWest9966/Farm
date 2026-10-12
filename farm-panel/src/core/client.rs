//! 契约客户端（ticket #22）：拼请求、解响应、解 WS 帧。
//!
//! 这是面板**唯一**知道路径的地方（ADR-0002 §1：面板是契约的另一个客户端）。
//! 传输层（wasm 侧的 `fetch` / `WebSocket`）只负责把 [`HttpRequest`] 发出去、
//! 把字节交回来给 [`ContractClient::decode`]，自己不拼 URL、不带 token。
//!
//! 纯函数、无 I/O：所以在无浏览器的宿主环境里可完整测试（见 `tests/contract_client.rs`）。

use farm_sdk::services::agent_runner::{
    AbortResponse, AttemptState, ClearQueueResponse, Disposition, SteerResponse,
};
use farm_sdk::services::events::{EventHistoryQuery, EventHistoryResponse};
use farm_sdk::services::tracker::{
    ClaimRequest, ClaimResponse, IssueListResponse, ReleaseRequest, ReleaseResponse,
};
use farm_sdk::services::workspace::MappingListResponse;
use farm_sdk::{
    ClientInfo, ClientKind, ContractError, EventEnvelope, HandshakeRequest, HandshakeResponse,
    WsHello, CONTRACT_VERSION, HANDSHAKE_PATH,
};

/// 面板在握手时自报的客户端名。
pub const PANEL_CLIENT_NAME: &str = "farm-panel";

/// op token header（`AuthScheme::default().http_header`）。
///
/// 宿主只认这一个 header（或 `Authorization: Bearer`），面板不另立标准。
pub const OP_TOKEN_HEADER: &str = "X-Farm-Token";

/// 契约 WS 事件流路由（`/v1/events/stream`）。
const EVENT_STREAM_PATH: &str = "/v1/events/stream";

/// 面板能发出的操作（闭集）。
///
/// 工单点名的 HITL 操作面全在这里：claim/release、steer/follow_up、abort、`clear_queue`；
/// 读只有三条（tracker 视图、workspace 映射、事件历史快照）+ WS 订阅。
/// **新增面板能力 = 先加契约路由，再加这里的变体**——顺序反了就不是契约客户端了。
#[derive(Clone, Debug, PartialEq)]
pub enum PanelOp {
    /// `POST /handshake`：声明契约版本与客户端身份。
    Handshake,
    /// `GET /v1/tracker/issues`：归一化 Issue 视图（tracker 是总线）。
    LoadIssues,
    /// `GET /v1/workspace/mappings`：spec ↔ 集成分支 ↔ worktrees 映射。
    LoadMappings,
    /// `GET /v1/events/history`：事件历史快照（`since` = 去重游标）。
    LoadEventHistory {
        since: Option<String>,
        ticket_id: Option<String>,
        limit: Option<u32>,
    },
    /// `GET /v1/events/stream`：WS 单连接订阅事件总线（增量推送）。
    OpenEventStream,
    /// `POST /v1/tracker/claims`：原子 claim（CAS 语义，assignee 即 claim）。
    Claim { issue_id: String, assignee: String },
    /// `POST /v1/tracker/releases`：交还排他所有权。
    Release { issue_id: String, assignee: String },
    /// `POST /v1/agent-runner/attempts/{id}/steer`：向运行中的 attempt 注入改道指令。
    Steer { attempt_id: String, message: String },
    /// `POST /v1/agent-runner/attempts/{id}/follow-up`：等 attempt 停下才送达的追加任务。
    FollowUp { attempt_id: String, message: String },
    /// `POST /v1/agent-runner/attempts/{id}/abort`：中止 attempt。
    Abort {
        attempt_id: String,
        reason: Option<String>,
    },
    /// `POST /v1/agent-runner/attempts/{id}/clear-queue`：清掉排队的注入。
    ClearQueue { attempt_id: String },
}

impl PanelOp {
    /// 人读的一句话（进操作日志与错误消息）。
    pub fn label(&self) -> &'static str {
        match self {
            PanelOp::Handshake => "handshake",
            PanelOp::LoadIssues => "load tracker issues",
            PanelOp::LoadMappings => "load workspace mappings",
            PanelOp::LoadEventHistory { .. } => "load event history",
            PanelOp::OpenEventStream => "open event stream",
            PanelOp::Claim { .. } => "claim",
            PanelOp::Release { .. } => "release",
            PanelOp::Steer { .. } => "steer",
            PanelOp::FollowUp { .. } => "follow_up",
            PanelOp::Abort { .. } => "abort",
            PanelOp::ClearQueue { .. } => "clear_queue",
        }
    }

    /// 是否是写操作（HITL 操作面；读操作失败只影响展示，不影响宿主状态）。
    pub fn is_mutation(&self) -> bool {
        matches!(
            self,
            PanelOp::Claim { .. }
                | PanelOp::Release { .. }
                | PanelOp::Steer { .. }
                | PanelOp::FollowUp { .. }
                | PanelOp::Abort { .. }
                | PanelOp::ClearQueue { .. }
        )
    }
}

/// 一条待发送的契约请求（传输层的唯一输入）。
#[derive(Clone, Debug, PartialEq)]
pub struct HttpRequest {
    /// `GET` / `POST`。
    pub method: &'static str,
    /// 契约路径（含 `/v1` 前缀；不含 base url）。
    pub path: String,
    /// 查询串参数（已按契约字段名摆好，未编码——见 [`encode_query_value`]）。
    pub query: Vec<(String, String)>,
    /// 请求头（op token + JSON content-type）。
    pub headers: Vec<(String, String)>,
    /// JSON 体（GET 为 `None`）。
    pub body: Option<String>,
}

impl HttpRequest {
    /// 拼出完整 URL（`base_url` + path + query）。
    pub fn url(&self, base_url: &str) -> String {
        let base = base_url.trim_end_matches('/');
        let mut url = format!("{base}{}", self.path);
        if !self.query.is_empty() {
            let query = self
                .query
                .iter()
                .map(|(k, v)| format!("{k}={}", encode_query_value(v)))
                .collect::<Vec<_>>()
                .join("&");
            url.push('?');
            url.push_str(&query);
        }
        url
    }
}

/// 一条契约响应（传输层交回来的原始字节）。
#[derive(Clone, Debug, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// 解码后的响应投影（与 [`PanelOp`] 一一对应）。
#[derive(Clone, Debug, PartialEq)]
pub enum Decoded {
    Handshake(HandshakeResponse),
    Issues(IssueListResponse),
    Mappings(MappingListResponse),
    History(EventHistoryResponse),
    Claimed(ClaimResponse),
    Released(ReleaseResponse),
    Steered(SteerResponse),
    Aborted(AbortResponse),
    QueueCleared(ClearQueueResponse),
}

/// 面板侧的错误：传输失败 / 响应不是契约形状 / 闭集错误信封。
#[derive(Clone, Debug, PartialEq)]
pub enum PanelError {
    /// 传输层就没把请求发出去（网络断了、DNS 挂了）。
    Transport(String),
    /// 响应体不是契约形状（坏 JSON / 成功状态却回了错误信封）。
    MalformedResponse {
        op: String,
        status: u16,
        detail: String,
    },
    /// WS 文本帧不是 hello / 事件 / 错误信封中的任何一种。
    MalformedFrame { detail: String },
    /// 宿主按契约闭集回答的错误（`unauthorized` / `conflict` / `not_found`…）。
    Contract(ContractError),
}

impl std::fmt::Display for PanelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PanelError::Transport(detail) => write!(f, "传输失败：{detail}"),
            PanelError::MalformedResponse { op, status, detail } => {
                write!(f, "{op} 的响应不是契约形状（HTTP {status}）：{detail}")
            }
            PanelError::MalformedFrame { detail } => write!(f, "WS 帧不是契约形状：{detail}"),
            PanelError::Contract(err) => write!(f, "{}：{}", err.code, err.message),
        }
    }
}

impl std::error::Error for PanelError {}

/// WS 上的一帧：首帧 hello、事件信封、或闭集错误信封。
#[derive(Clone, Debug, PartialEq)]
pub enum WsFrame {
    Hello(WsHello),
    Event(EventEnvelope),
    Error(ContractError),
}

/// 解一条 WS 文本帧。
///
/// 三种形状的判别字段互斥（`type` / `code` / 事件信封字段），所以先解成 `Value` 再分流，
/// 不靠「猜一个类型失败了试下一个」。
pub fn parse_ws_frame(text: &str) -> Result<WsFrame, PanelError> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| PanelError::MalformedFrame {
        detail: e.to_string(),
    })?;

    if value.get("type").and_then(|t| t.as_str()) == Some("hello") {
        let hello: WsHello = serde_json::from_value(value)
            .map_err(|e| PanelError::MalformedFrame { detail: e.to_string() })?;
        return Ok(WsFrame::Hello(hello));
    }
    if value.get("code").is_some() {
        let err: ContractError = serde_json::from_value(value)
            .map_err(|e| PanelError::MalformedFrame { detail: e.to_string() })?;
        return Ok(WsFrame::Error(err));
    }
    let envelope: EventEnvelope =
        serde_json::from_value(value).map_err(|e| PanelError::MalformedFrame {
            detail: e.to_string(),
        })?;
    Ok(WsFrame::Event(envelope))
}

/// 契约客户端：持有 base url 与 op token，负责拼请求与解响应。
///
/// token 只在内存里（使用者在页面输入，不落盘——ADR-0003 §4 面板是静态资产，不带 token）。
#[derive(Clone, Debug)]
pub struct ContractClient {
    base_url: String,
    op_token: String,
}

impl ContractClient {
    pub fn new(base_url: impl Into<String>, op_token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            op_token: op_token.into(),
        }
    }

    /// 宿主 base url（`http://127.0.0.1:3721` 之类）。
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 握手请求（面板自报 `ClientKind::Panel`）。
    pub fn handshake_request(&self) -> HandshakeRequest {
        HandshakeRequest {
            contract_version: CONTRACT_VERSION.to_string(),
            client: ClientInfo {
                name: PANEL_CLIENT_NAME.to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                kind: ClientKind::Panel,
            },
        }
    }

    /// WS 订阅 URL（契约事件流路由）。
    pub fn stream_url(&self) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            EVENT_STREAM_PATH.trim_start_matches('/')
        )
    }

    /// 把一个操作拼成契约请求。
    pub fn request_for(&self, op: &PanelOp) -> HttpRequest {
        match op {
            PanelOp::Handshake => self.post_json(HANDSHAKE_PATH, &self.handshake_request()),
            PanelOp::LoadIssues => self.get("/v1/tracker/issues"),
            PanelOp::LoadMappings => self.get("/v1/workspace/mappings"),
            PanelOp::LoadEventHistory {
                since,
                ticket_id,
                limit,
            } => {
                let mut query = Vec::new();
                if let Some(since) = since {
                    query.push(("since".to_string(), since.clone()));
                }
                if let Some(ticket_id) = ticket_id {
                    query.push(("ticket_id".to_string(), ticket_id.clone()));
                }
                if let Some(limit) = limit {
                    query.push(("limit".to_string(), limit.to_string()));
                }
                HttpRequest {
                    method: "GET",
                    path: "/v1/events/history".to_string(),
                    query,
                    headers: self.headers(false),
                    body: None,
                }
            }
            PanelOp::OpenEventStream => HttpRequest {
                method: "GET",
                path: EVENT_STREAM_PATH.to_string(),
                query: Vec::new(),
                headers: self.headers(false),
                body: None,
            },
            PanelOp::Claim { issue_id, assignee } => self.post_json(
                "/v1/tracker/claims",
                &ClaimRequest {
                    issue_id: issue_id.clone(),
                    assignee: assignee.clone(),
                },
            ),
            PanelOp::Release { issue_id, assignee } => self.post_json(
                "/v1/tracker/releases",
                &ReleaseRequest {
                    issue_id: issue_id.clone(),
                    assignee: assignee.clone(),
                },
            ),
            // 注入路由的 attempt_id **以路径为准**（同 farm-host 的 steer/abort handler）：
            // 请求体只带消息本身，id 在路径里再抄一遍没有意义。
            PanelOp::Steer { attempt_id, message } => self.post_json(
                &format!("/v1/agent-runner/attempts/{attempt_id}/steer"),
                &serde_json::json!({ "message": message }),
            ),
            PanelOp::FollowUp { attempt_id, message } => self.post_json(
                &format!("/v1/agent-runner/attempts/{attempt_id}/follow-up"),
                &serde_json::json!({ "message": message }),
            ),
            PanelOp::Abort { attempt_id, reason } => self.post_json(
                &format!("/v1/agent-runner/attempts/{attempt_id}/abort"),
                &match reason {
                    Some(reason) => serde_json::json!({ "reason": reason }),
                    None => serde_json::json!({}),
                },
            ),
            PanelOp::ClearQueue { attempt_id } => self.post_json(
                &format!("/v1/agent-runner/attempts/{attempt_id}/clear-queue"),
                &serde_json::json!({}),
            ),
        }
    }

    /// 解一条响应：2xx → 投影；非 2xx → 闭集错误信封。
    pub fn decode(
        &self,
        op: &PanelOp,
        response: &HttpResponse,
    ) -> Result<Decoded, PanelError> {
        if !(200..300).contains(&response.status) {
            return match serde_json::from_slice::<ContractError>(&response.body) {
                Ok(err) => Err(PanelError::Contract(err)),
                Err(e) => Err(PanelError::MalformedResponse {
                    op: op.label().to_string(),
                    status: response.status,
                    detail: e.to_string(),
                }),
            };
        }

        let malformed = |e: serde_json::Error| PanelError::MalformedResponse {
            op: op.label().to_string(),
            status: response.status,
            detail: e.to_string(),
        };

        match op {
            PanelOp::Handshake => serde_json::from_slice(&response.body)
                .map(Decoded::Handshake)
                .map_err(malformed),
            PanelOp::LoadIssues => serde_json::from_slice(&response.body)
                .map(Decoded::Issues)
                .map_err(malformed),
            PanelOp::LoadMappings => serde_json::from_slice(&response.body)
                .map(Decoded::Mappings)
                .map_err(malformed),
            PanelOp::LoadEventHistory { .. } => serde_json::from_slice(&response.body)
                .map(Decoded::History)
                .map_err(malformed),
            PanelOp::Claim { .. } => serde_json::from_slice(&response.body)
                .map(Decoded::Claimed)
                .map_err(malformed),
            PanelOp::Release { .. } => serde_json::from_slice(&response.body)
                .map(Decoded::Released)
                .map_err(malformed),
            PanelOp::Steer { .. } | PanelOp::FollowUp { .. } => serde_json::from_slice(&response.body)
                .map(Decoded::Steered)
                .map_err(malformed),
            PanelOp::Abort { .. } => serde_json::from_slice(&response.body)
                .map(Decoded::Aborted)
                .map_err(malformed),
            PanelOp::ClearQueue { .. } => serde_json::from_slice(&response.body)
                .map(Decoded::QueueCleared)
                .map_err(malformed),
            // WS 升级不走 HTTP 解码路径。
            PanelOp::OpenEventStream => Err(PanelError::MalformedResponse {
                op: op.label().to_string(),
                status: response.status,
                detail: "the event stream is a websocket, not an HTTP response".to_string(),
            }),
        }
    }

    /// 事件历史查询的契约形状（给 `Rhythm` 与测试复用）。
    pub fn history_query(
        since: Option<String>,
        ticket_id: Option<String>,
        limit: Option<u32>,
    ) -> EventHistoryQuery {
        EventHistoryQuery {
            since,
            kind: None,
            ticket_id,
            limit,
        }
    }

    fn get(&self, path: &str) -> HttpRequest {
        HttpRequest {
            method: "GET",
            path: path.to_string(),
            query: Vec::new(),
            headers: self.headers(false),
            body: None,
        }
    }

    fn post_json(&self, path: &str, body: &impl serde::Serialize) -> HttpRequest {
        HttpRequest {
            method: "POST",
            path: path.to_string(),
            query: Vec::new(),
            headers: self.headers(true),
            body: Some(serde_json::to_string(body).expect("contract request must serialize")),
        }
    }

    /// 统一的头：op token（所有契约路由都要验）+ JSON content-type（有体才带）。
    fn headers(&self, json_body: bool) -> Vec<(String, String)> {
        let mut headers = vec![(OP_TOKEN_HEADER.to_string(), self.op_token.clone())];
        if json_body {
            headers.push(("content-type".to_string(), "application/json".to_string()));
        }
        headers
    }
}

/// 查询值编码（percent-encoding 保留集之外的字符）。
///
/// 只用于 `since` / `ticket_id` / `limit`：RFC3339 时间戳与票号都不含保留字符，
/// 但编码一次比「假设客户端永远乖巧」便宜。
pub fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        let safe = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~');
        if safe {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// disposition 的人读形式（面板回显用）。
pub fn disposition_label(disposition: &Disposition) -> String {
    if disposition.is_known() {
        disposition.to_string()
    } else {
        // 未知取值 = 版本偏差：照原样显示，不断失败（GLOSSARY：记录偏差）。
        format!("{}（未知取值，版本偏差）", disposition)
    }
}

/// attempt 状态的人读形式。
pub fn attempt_state_label(state: AttemptState) -> &'static str {
    match state {
        AttemptState::Running => "running",
        AttemptState::Settled => "settled",
        AttemptState::Failed => "failed",
        AttemptState::Interrupted => "interrupted",
        AttemptState::Aborted => "aborted",
    }
}
