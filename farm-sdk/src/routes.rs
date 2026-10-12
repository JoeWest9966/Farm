//! 契约路由清单：路径的**单一源**。
//!
//! 宿主按 `CONTRACT_ROUTES` / `ADMIN_ROUTES` 挂路由，生成文档按它出路由表，
//! 编排器与面板按它拼 URL。改路径 = 改契约，必须三处同步（由本表保证）。

use serde::Serialize;

use crate::handshake::{ADMIN_PREFIX, CONTRACT_PREFIX, HANDSHAKE_PATH};
use crate::services::ServiceName;

/// 一条契约路由。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RouteSpec {
    pub service: ServiceName,
    /// HTTP 方法名（`GET` / `POST`）。
    pub method: &'static str,
    /// 完整路径（含 `/v1` 前缀）。
    pub path: &'static str,
    /// 一句话说明（进生成文档）。
    pub summary: &'static str,
}

/// v1 契约路由（占位）。除 `GET /v1/events/stream`（WS）外全部返回闭集
/// `not_implemented`，直到对应服务票落地。
pub const CONTRACT_ROUTES: &[RouteSpec] = &[
    // --- Tracker 服务 ---
    RouteSpec {
        service: ServiceName::Tracker,
        method: "GET",
        path: "/v1/tracker/issues",
        summary: "归一化 Issue 视图（面板与编排器读同一视图）",
    },
    RouteSpec {
        service: ServiceName::Tracker,
        method: "POST",
        path: "/v1/tracker/claims",
        summary: "原子 claim（CAS 语义，assignee 即 claim）",
    },
    RouteSpec {
        service: ServiceName::Tracker,
        method: "POST",
        path: "/v1/tracker/releases",
        summary: "release：交还排他所有权（claim 是票据状态，不是技术锁）",
    },
    // --- Agent-Runner ---
    RouteSpec {
        service: ServiceName::AgentRunner,
        method: "POST",
        path: "/v1/agent-runner/dispatches",
        summary: "派发 agent（本地后端 spawn pi --mode rpc）",
    },
    RouteSpec {
        service: ServiceName::AgentRunner,
        method: "POST",
        path: "/v1/agent-runner/attempts/{attempt_id}/steer",
        summary: "向运行中的 attempt 注入指令",
    },
    RouteSpec {
        service: ServiceName::AgentRunner,
        method: "POST",
        path: "/v1/agent-runner/attempts/{attempt_id}/abort",
        summary: "中止 attempt（abort 探针）",
    },
    RouteSpec {
        service: ServiceName::AgentRunner,
        method: "POST",
        path: "/v1/agent-runner/attempts/{attempt_id}/follow-up",
        summary: "follow_up：等 attempt 停下才送达的追加任务",
    },
    RouteSpec {
        service: ServiceName::AgentRunner,
        method: "POST",
        path: "/v1/agent-runner/attempts/{attempt_id}/clear-queue",
        summary: "clear_queue：清掉 attempt 上排队的注入，回报清了几条",
    },
    // --- Merge 服务 ---
    RouteSpec {
        service: ServiceName::Merge,
        method: "POST",
        path: "/v1/merge/proposals",
        summary: "agent 提议 merge（宿主校验后执行）",
    },
    // --- Workspace 服务 ---
    RouteSpec {
        service: ServiceName::Workspace,
        method: "GET",
        path: "/v1/workspace/mappings",
        summary: "spec ↔ 集成分支 ↔ worktrees 映射",
    },
    // --- 事件总线 ---
    RouteSpec {
        service: ServiceName::Events,
        method: "GET",
        path: "/v1/events/stream",
        summary: "WS 单连接订阅事件总线（握手同样验 token）",
    },
    RouteSpec {
        service: ServiceName::Events,
        method: "GET",
        path: "/v1/events/history",
        summary: "事件历史查询（有界保留窗口）",
    },
];

/// 宿主管理 API 路由（占位，独立前缀 `/admin`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct AdminRouteSpec {
    pub method: &'static str,
    pub path: &'static str,
    pub summary: &'static str,
}

/// 宿主管理 API 路由（占位）。插件装卸归 #19，服务启停/tracker 配置归 #23。
pub const ADMIN_ROUTES: &[AdminRouteSpec] = &[
    AdminRouteSpec {
        method: "GET",
        path: "/admin/plugins",
        summary: "插件清单与状态（#19 监督状态 → PluginSummary）",
    },
    AdminRouteSpec {
        method: "POST",
        path: "/admin/plugins/{id}/recover",
        summary: "恢复隔离/停机的插件（GLOSSARY「隔离」一键恢复，#19 recover）",
    },
    AdminRouteSpec {
        method: "GET",
        path: "/admin/status",
        summary: "宿主管理面自描述（契约版本 + 五服务）",
    },
    AdminRouteSpec {
        method: "GET",
        path: "/admin/tracker/provider",
        summary: "查询当前 tracker provider / adapter 配置（adapter 选择，#23）",
    },
    AdminRouteSpec {
        method: "POST",
        path: "/admin/tracker/provider",
        summary: "设置 tracker provider / adapter（adapter 选择，#23）",
    },
    AdminRouteSpec {
        method: "GET",
        path: "/admin/services",
        summary: "五服务启停状态（服务启停控制面，#23）",
    },
    AdminRouteSpec {
        method: "POST",
        path: "/admin/services/{name}/stop",
        summary: "停机一个服务（服务启停，#23；宿主持有开关，发 needs_attention）",
    },
    AdminRouteSpec {
        method: "POST",
        path: "/admin/services/{name}/start",
        summary: "启动一个服务（服务启停，#23）",
    },
    AdminRouteSpec {
        method: "GET",
        path: "/admin/workflow/config/{repo_id}",
        summary: "查询某被编排仓库当前生效的 workflow 配置与 LKG 状态（ticket #12）",
    },
];

/// 拼接某服务在契约前缀下的路径。
pub fn contract_path(service: ServiceName, suffix: &str) -> String {
    format!("{}/{}{}", CONTRACT_PREFIX, service.path_segment(), suffix)
}

/// 拼接宿主管理 API 路径。
pub fn admin_path(suffix: &str) -> String {
    format!("{}{}", ADMIN_PREFIX, suffix)
}

/// 非路由的契约面端点（握手路由与面板根路径）。
pub const HANDSHAKE_ROUTE: (&str, &str) = ("POST", HANDSHAKE_PATH);
