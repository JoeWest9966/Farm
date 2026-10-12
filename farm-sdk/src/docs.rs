//! 契约文档生成器：类型是单一源，文档是生成物。
//!
//! ADR-0001 决策 6：schema 单一源 + 生成文档 + 闭集错误码 + 一致性套件是
//! 「符合契约」的唯一凭证。本模块把 `farm-sdk` 的类型渲染成
//! `docs/contract/schema.json`（JSON Schema 2020-12）与 `docs/contract/README.md`
//! （路由表 + 闭集清单）。测试断言已签入文件与渲染结果逐字节一致。

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use serde::Serialize;

use crate::admin::{AdminPluginListResponse, AdminStatusResponse, PluginState, PluginSummary};
use crate::control::{TickControlRequest, TickControlStatus, TickState};
use crate::error::{ContractError, ErrorCode};
use crate::events::{EventEnvelope, EventKind, NewEvent, WsHello};
use crate::handshake::{
    AuthScheme, ClientInfo, ClientKind, HandshakeRequest, HandshakeResponse, ADMIN_PREFIX,
    CONTRACT_MAJOR, CONTRACT_PREFIX, CONTRACT_VERSION, HANDSHAKE_PATH, HOST_VERSION,
};
use crate::overlay::{
    OverlayClearRequest, OverlayClearResponse, OverlayEntry, OverlayKey, OverlaySetRequest,
    OverlayStatus, OverlayValueKind,
};
use crate::routes::{AdminRouteSpec, RouteSpec, ADMIN_ROUTES, CONTRACT_ROUTES};
use crate::workflow::{
    EffectiveSource, LastKnownGoodInfo, ProjectionSwitches, RetryBackoff, WorkflowConfig,
    WorkflowConfigStatus,
};
use crate::services::ServiceName;
use crate::services::agent_runner::{
    AbortRequest, AttemptState, ClearQueueResponse, DispatchRequest, DispatchResponse, SteerRequest,
};
use crate::services::events::{EventHistoryQuery, EventHistoryResponse};
use crate::services::merge::{MergeProposal, MergeProposalResponse, MergeProposalState};
use crate::services::tracker::{
    ClaimRequest, ClaimResponse, IssueListResponse, IssueState, IssueView, ReleaseOutcome,
    ReleaseRequest, ReleaseResponse,
};
use crate::services::workspace::{MappingListResponse, WorktreeRef, WorktreeState, WorkspaceMapping};

/// 生成物目录（相对 workspace 根）。`farm-sdk` 的 manifest dir 是 `<root>/farm-sdk`。
pub static DOCS_DIR: LazyLock<PathBuf> =
    LazyLock::new(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/contract"));

/// 宿主已经真正打通的契约路由（其余全部返回闭集 `not_implemented`）。
///
/// 随各服务票落地而增长：tracker 两条（#13）、workspace 映射（#14）、事件投影两条（#15）。
pub const SKELETON_LIVE_ROUTES: &[&str] = &[
    "/v1/tracker/issues",
    "/v1/tracker/claims",
    "/v1/tracker/releases",
    "/v1/workspace/mappings",
    "/v1/events/stream",
    "/v1/events/history",
    "/v1/agent-runner/dispatches",
    "/v1/agent-runner/attempts/{attempt_id}/steer",
    "/v1/agent-runner/attempts/{attempt_id}/abort",
    "/v1/agent-runner/attempts/{attempt_id}/follow-up",
    "/v1/agent-runner/attempts/{attempt_id}/clear-queue",
];

/// 渲染全部契约文档：`文件名 -> 内容`。
pub fn render_all() -> std::collections::BTreeMap<&'static str, String> {
    let mut out = std::collections::BTreeMap::new();
    out.insert("schema.json", render_schema_json());
    out.insert("README.md", render_readme_md());
    out
}

/// 渲染 `schema.json`：一份 JSON Schema 2020-12 文档覆盖整个契约面。
pub fn render_schema_json() -> String {
    let schema = contract_schema();
    let mut json = serde_json::to_string_pretty(&schema).expect("schema must serialize");
    json.push('\n');
    json
}

fn schema_value<T: schemars::JsonSchema>() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schemars schema must serialize")
}

/// 所有进 `$defs` 的契约类型（显式清单：漏一个就是漏一份契约）。
fn named_type_schemas() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("HandshakeRequest", schema_value::<HandshakeRequest>()),
        ("HandshakeResponse", schema_value::<HandshakeResponse>()),
        ("ClientInfo", schema_value::<ClientInfo>()),
        ("ClientKind", schema_value::<ClientKind>()),
        ("AuthScheme", schema_value::<AuthScheme>()),
        ("ErrorCode", schema_value::<ErrorCode>()),
        ("ContractError", schema_value::<ContractError>()),
        ("ServiceName", schema_value::<ServiceName>()),
        ("EventKind", schema_value::<EventKind>()),
        ("NewEvent", schema_value::<NewEvent>()),
        ("EventEnvelope", schema_value::<EventEnvelope>()),
        ("WsHello", schema_value::<WsHello>()),
        ("IssueState", schema_value::<IssueState>()),
        ("IssueView", schema_value::<IssueView>()),
        ("IssueListResponse", schema_value::<IssueListResponse>()),
        ("ClaimRequest", schema_value::<ClaimRequest>()),
        ("ClaimResponse", schema_value::<ClaimResponse>()),
        ("ReleaseRequest", schema_value::<ReleaseRequest>()),
        ("ReleaseResponse", schema_value::<ReleaseResponse>()),
        ("ReleaseOutcome", schema_value::<ReleaseOutcome>()),
        ("ClearQueueResponse", schema_value::<ClearQueueResponse>()),
        ("AttemptState", schema_value::<AttemptState>()),
        ("DispatchRequest", schema_value::<DispatchRequest>()),
        ("DispatchResponse", schema_value::<DispatchResponse>()),
        ("SteerRequest", schema_value::<SteerRequest>()),
        ("AbortRequest", schema_value::<AbortRequest>()),
        ("MergeProposalState", schema_value::<MergeProposalState>()),
        ("MergeProposal", schema_value::<MergeProposal>()),
        ("MergeProposalResponse", schema_value::<MergeProposalResponse>()),
        ("WorktreeState", schema_value::<WorktreeState>()),
        ("WorktreeRef", schema_value::<WorktreeRef>()),
        ("WorkspaceMapping", schema_value::<WorkspaceMapping>()),
        ("MappingListResponse", schema_value::<MappingListResponse>()),
        ("EventHistoryQuery", schema_value::<EventHistoryQuery>()),
        ("EventHistoryResponse", schema_value::<EventHistoryResponse>()),
        ("PluginState", schema_value::<PluginState>()),
        ("PluginSummary", schema_value::<PluginSummary>()),
        ("AdminPluginListResponse", schema_value::<AdminPluginListResponse>()),
        ("AdminStatusResponse", schema_value::<AdminStatusResponse>()),
        ("WorkflowConfig", schema_value::<WorkflowConfig>()),
        ("RetryBackoff", schema_value::<RetryBackoff>()),
        ("ProjectionSwitches", schema_value::<ProjectionSwitches>()),
        ("EffectiveSource", schema_value::<EffectiveSource>()),
        ("LastKnownGoodInfo", schema_value::<LastKnownGoodInfo>()),
        ("WorkflowConfigStatus", schema_value::<WorkflowConfigStatus>()),
        ("OverlayKey", schema_value::<OverlayKey>()),
        ("OverlayValueKind", schema_value::<OverlayValueKind>()),
        ("OverlayEntry", schema_value::<OverlayEntry>()),
        ("OverlaySetRequest", schema_value::<OverlaySetRequest>()),
        ("OverlayClearRequest", schema_value::<OverlayClearRequest>()),
        ("OverlayClearResponse", schema_value::<OverlayClearResponse>()),
        ("OverlayStatus", schema_value::<OverlayStatus>()),
        ("TickState", schema_value::<TickState>()),
        ("TickControlStatus", schema_value::<TickControlStatus>()),
        ("TickControlRequest", schema_value::<TickControlRequest>()),
    ]
}

/// 把每个类型的 `$defs` 上提到文档根，类型自身成为根 `$defs` 的一项。
fn contract_schema() -> serde_json::Value {
    let mut defs = serde_json::Map::new();
    for (name, schema) in named_type_schemas() {
        let mut schema = schema;
        if let Some(obj) = schema.as_object_mut() {
            // 嵌套 $defs 上提；$schema 只出现在文档根
            if let Some(serde_json::Value::Object(inner)) = obj.remove("$defs") {
                for (k, v) in inner {
                    defs.entry(k).or_insert(v);
                }
            }
            obj.remove("$schema");
        }
        defs.insert(name.to_string(), schema);
    }

    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://raw.githubusercontent.com/JoeWest9966/Farm/main/docs/contract/schema.json",
        "title": "Farm orchestrator contract",
        "description": "GENERATED from farm-sdk types (single source of truth). Do not edit by hand; regenerate with `cargo run -p farm-sdk --example gen_contract_docs`.",
        "x-farm-generated": true,
        "x-farm-generator": "cargo run -p farm-sdk --example gen_contract_docs",
        "x-farm-contract-version": CONTRACT_VERSION,
        "x-farm-contract-major": CONTRACT_MAJOR,
        "type": "object",
        "properties": {
            "contractVersion": { "const": CONTRACT_VERSION },
            "handshake": {
                "type": "object",
                "properties": {
                    "method": { "const": "POST" },
                    "path": { "const": HANDSHAKE_PATH },
                    "request": { "$ref": "#/$defs/HandshakeRequest" },
                    "response": { "$ref": "#/$defs/HandshakeResponse" },
                    "error": { "$ref": "#/$defs/ContractError" }
                },
                "required": ["method", "path", "request", "response", "error"],
                "additionalProperties": false
            },
            "auth": { "$ref": "#/$defs/AuthScheme" },
            "services": {
                "type": "array",
                "items": { "$ref": "#/$defs/ServiceName" },
                "description": "v1 五服务（ADR-0001）"
            },
            "routes": {
                "type": "array",
                "items": route_schema(),
                "description": "编排器契约路由（占位形状，宿主按本表挂路由）"
            },
            "adminRoutes": {
                "type": "array",
                "items": admin_route_schema(),
                "description": "宿主管理 API 路由（独立前缀 /admin，接口位）"
            },
            "errorCodes": {
                "type": "array",
                "items": { "$ref": "#/$defs/ErrorCode" },
                "description": "闭集错误码：HTTP 与 WS 共用同一信封"
            },
            "errorEnvelope": { "$ref": "#/$defs/ContractError" },
            "eventKinds": {
                "type": "array",
                "items": { "$ref": "#/$defs/EventKind" },
                "description": "事件闭集（加法演进）"
            },
            "eventEnvelope": { "$ref": "#/$defs/EventEnvelope" },
            "newEvent": { "$ref": "#/$defs/NewEvent" },
            "wsHello": { "$ref": "#/$defs/WsHello" }
        },
        "required": [
            "contractVersion", "handshake", "auth", "services",
            "routes", "adminRoutes", "errorCodes", "eventKinds"
        ],
        "additionalProperties": false,
        "$defs": serde_json::Value::Object(defs)
    })
}

fn route_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "service": { "$ref": "#/$defs/ServiceName" },
            "method": { "type": "string", "enum": ["GET", "POST"] },
            "path": { "type": "string", "pattern": "^/v1/" },
            "summary": { "type": "string" }
        },
        "required": ["service", "method", "path", "summary"],
        "additionalProperties": false
    })
}

fn admin_route_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "method": { "type": "string", "enum": ["GET", "POST"] },
            "path": { "type": "string", "pattern": "^/admin/" },
            "summary": { "type": "string" }
        },
        "required": ["method", "path", "summary"],
        "additionalProperties": false
    })
}

/// 渲染 `README.md`：路由表 + 闭集清单（人读的契约摘要）。
pub fn render_readme_md() -> String {
    let mut md = String::new();
    md.push_str("# Farm 编排器契约（orchestrator contract）\n\n");
    md.push_str("> **GENERATED — 勿手改。** 本文件由 `farm-sdk` 类型单一源渲染：\n");
    md.push_str("> `cargo run -p farm-sdk --example gen_contract_docs`\n");
    md.push_str(
        "> 测试 `farm-sdk/tests/contract_docs_are_generated.rs` 断言本文件与生成结果逐字节一致。\n\n",
    );
    md.push_str(&format!("- 契约版本（contract version）：`{CONTRACT_VERSION}`（主版本号 `{CONTRACT_MAJOR}`，握手只比主版本）\n"));
    md.push_str(&format!("- 宿主版本（host version）：`{HOST_VERSION}`\n"));
    md.push_str(&format!("- farm-sdk 版本：`{}`\n\n", crate::SDK_VERSION));

    md.push_str("## 鉴权（auth）\n\n");
    md.push_str("单一 op token，静态配置。**所有契约路由（含 `/handshake`）与 WS upgrade 都要验**；\n");
    md.push_str("缺 token 与错 token 返回**同一个**闭集错误 `unauthorized`（HTTP 401，WS upgrade 同样 401 + 同一 JSON 体）。\n\n");
    md.push_str("| 面 | header | 说明 |\n");
    md.push_str("| --- | --- | --- |\n");
    md.push_str("| 契约 HTTP | `X-Farm-Token: <token>` | 或 `Authorization: Bearer <token>` |\n");
    md.push_str("| 契约 WS upgrade | 同上 | 握手同样验 token |\n");
    md.push_str("| 宿主管理 API | `X-Farm-Admin-Token: <token>` | 独立 token，与 op token 不是同一个 |\n\n");

    md.push_str("## 握手（handshake）\n\n");
    md.push_str(&format!("`POST {HANDSHAKE_PATH}` — 编排器接入第一跳。\n\n"));
    md.push_str("- 请求：`HandshakeRequest`（`contract_version` + `client`）\n");
    md.push_str("- 响应：`HandshakeResponse`（契约版本、五服务清单、事件闭集、鉴权方式）\n");
    md.push_str("- 主版本不符 → 闭集错误 `contract_version_unsupported`（HTTP 400）\n\n");

    md.push_str("## 契约路由（v1 五服务）\n\n");
    md.push_str("| 服务 | 方法 | 路径 | 说明 | 本票状态 |\n");
    md.push_str("| --- | --- | --- | --- | --- |\n");
    for route in CONTRACT_ROUTES {
        let status = if SKELETON_LIVE_ROUTES.contains(&route.path) {
            match route.path {
                "/v1/events/stream" => "已通：WS 单连接事件投影（#15）",
                "/v1/events/history" => "已通：事件历史快照 API（#15）",
                "/v1/tracker/issues" => "已通：归一化 Issue 视图（#13）",
                "/v1/tracker/claims" => "已通：原子 claim（#13）",
                "/v1/workspace/mappings" => "已通：workspace 映射视图（#14）",
                "/v1/agent-runner/dispatches" => "已通：派发 attempt（#16）",
                "/v1/agent-runner/attempts/{attempt_id}/steer" => "已通：steer 注入（#16）",
                "/v1/agent-runner/attempts/{attempt_id}/abort" => "已通：abort 探针（#16）",
                "/v1/agent-runner/attempts/{attempt_id}/follow-up" => "已通：follow_up 注入（#22）",
                "/v1/agent-runner/attempts/{attempt_id}/clear-queue" => "已通：clear_queue（#22）",
                "/v1/tracker/releases" => "已通：release 交还排他所有权（#22）",
                _ => "已通",
            }
        } else {
            "占位：`not_implemented`"
        };
        md.push_str(&format!(
            "| {} | {} | `{}` | {} | {} |\n",
            route.service, route.method, route.path, route.summary, status
        ));
    }
    md.push('\n');

    md.push_str("## 宿主管理 API（host admin API，接口位）\n\n");
    md.push_str(&format!("独立前缀 `{ADMIN_PREFIX}`，独立 token header `X-Farm-Admin-Token`。\n"));
    md.push_str("把 op token 递到管理面同样得到 `unauthorized`——两个 token 不是同一个。\n\n");
    md.push_str("| 方法 | 路径 | 说明 | 本票状态 |\n");
    md.push_str("| --- | --- | --- | --- |\n");
    for route in ADMIN_ROUTES {
        md.push_str(&format!(
            "| {} | `{}` | {} | 占位：`not_implemented` |\n",
            route.method, route.path, route.summary
        ));
    }
    md.push('\n');

    md.push_str("## 闭集错误码（closed-set error codes）\n\n");
    md.push_str("HTTP 响应体与 WS 文本帧共用同一信封：\n\n");
    md.push_str("```json\n{\"code\":\"not_implemented\",\"message\":\"...\",\"retryable\":false}\n```\n\n");
    md.push_str("| code | HTTP | 可重试 | 含义 |\n");
    md.push_str("| --- | --- | --- | --- |\n");
    for code in ErrorCode::ALL {
        let meaning = error_meaning(code);
        md.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            code.as_str(),
            code.http_status(),
            if code.is_retryable() { "是" } else { "否" },
            meaning
        ));
    }
    md.push('\n');

    md.push_str("## 事件闭集（event closed set）\n\n");
    md.push_str("事件信封 `EventEnvelope`；`audited=false` 表示未入 Kron 账本（ADR-0005 §8 启动补账）。\n\n");
    md.push_str("| kind | 通知闭包 | 审计双写 | 含义 |\n");
    md.push_str("| --- | --- | --- | --- |\n");
    for kind in EventKind::ALL {
        md.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            kind.as_str(),
            if kind.is_notification_critical() { "是" } else { "否" },
            if kind.is_audit_critical() { "是" } else { "否" },
            event_meaning(kind)
        ));
    }
    md.push('\n');

    md.push_str("## Schema\n\n");
    md.push_str("机器可读的单一源在 [`schema.json`](./schema.json)（JSON Schema 2020-12，`$defs` 覆盖全部契约类型）。\n");
    md.push_str("宿主私有存储的 schema **永不**进这里——SQLite 是宿主私有实现细节（ADR-0005 §5）。\n");
    md
}

fn error_meaning(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Unauthorized => "缺 token / 错 token（同码，不做区分）",
        ErrorCode::Forbidden => "已鉴权但无权限",
        ErrorCode::ContractVersionUnsupported => "握手契约版本不被支持",
        ErrorCode::InvalidRequest => "请求不符合契约 schema",
        ErrorCode::NotFound => "路由不存在",
        ErrorCode::MethodNotAllowed => "路由存在但方法不对",
        ErrorCode::Conflict => "状态冲突（如原子 claim 已被抢走）",
        ErrorCode::NotImplemented => "路由已占位、服务未实现",
        ErrorCode::ServiceUnavailable => "宿主暂时不可用",
        ErrorCode::Internal => "宿主内部错误",
    }
}

fn event_meaning(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Dispatch => "派发：agent-runner 起了 attempt",
        EventKind::AttemptStarted => "attempt 生命周期占位：已启动",
        EventKind::AgentSettled => "attempt 生命周期占位：判据阶梯判终态",
        EventKind::AttemptFinished => "attempt 生命周期占位：已收尾",
        EventKind::Blocked => "一等事件：attempt 卡住，claim 保留",
        EventKind::NeedsAttention => "一等事件：需要人类注意",
        EventKind::Steer => "向运行中的 attempt 注入指令",
        EventKind::Abort => "中止 attempt",
        EventKind::StallKill => "两口钟超时后判杀",
        EventKind::Merge => "merge 服务执行了真正的 merge",
        EventKind::Approval => "人工闸门放行 merge 提议",
        EventKind::Interrupted => "宿主重启标记的第四终态",
        EventKind::HostRestarted => "宿主重启（崩溃恢复）",
        EventKind::ClaimAcquired => "原子 claim 成功",
        EventKind::PluginQuarantined => "插件重试耗尽进入隔离",
        EventKind::NotificationSent => "通知通道外发完成",
    }
}

/// 生成器自己的路由表序列化（给 orchestrator-skeleton 与一致性套件复用）。
#[derive(Serialize)]
pub struct RouteTable {
    pub contract_version: String,
    pub handshake_path: String,
    pub contract_prefix: String,
    pub admin_prefix: String,
    pub contract_routes: Vec<RouteSpec>,
    pub admin_routes: Vec<AdminRouteSpec>,
    pub services: Vec<ServiceName>,
    pub event_kinds: Vec<EventKind>,
    pub error_codes: Vec<ErrorCode>,
}

impl RouteTable {
    pub fn current() -> Self {
        Self {
            contract_version: CONTRACT_VERSION.to_string(),
            handshake_path: HANDSHAKE_PATH.to_string(),
            contract_prefix: CONTRACT_PREFIX.to_string(),
            admin_prefix: ADMIN_PREFIX.to_string(),
            contract_routes: CONTRACT_ROUTES.to_vec(),
            admin_routes: ADMIN_ROUTES.to_vec(),
            services: ServiceName::ALL.to_vec(),
            event_kinds: EventKind::ALL.to_vec(),
            error_codes: ErrorCode::ALL.to_vec(),
        }
    }
}
