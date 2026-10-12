//! 宿主管理面客户端（ticket #25）：面板经 **ADMIN API**（管理 token）操作宿主自身 +
//! merge 审阅。ADR-0002 §1：管理面与编排器契约边界分明——独立 `/admin` 前缀 + 独立
//! `X-Farm-Admin-Token`。这是面板**唯一**知道管理面路径的地方；纯函数、无 I/O，
//! 在无浏览器的宿主环境里可完整测试（见 `tests/admin_client.rs`）。

use farm_sdk::admin::{
    AdminPluginListResponse, AdminStatusResponse, PluginSummary, ServiceControlStatus,
    ServiceControlStatusEntry, TrackerProviderConfig, TrackerProviderSelection,
};
use farm_sdk::services::merge::{
    MergeProposalDetail, MergeProposalListResponse, MergeReviewResponse,
};
use farm_sdk::ContractError;

use super::client::{HttpRequest, HttpResponse, PanelError};

/// 管理 token header（`AuthScheme::default().admin_header`；管理 token ≠ 操作 token）。
pub const ADMIN_TOKEN_HEADER: &str = "X-Farm-Admin-Token";

/// 管理面操作（面板 HITL 经管理 API；merge 审阅 + 宿主自身管理）。
#[derive(Clone, Debug, PartialEq)]
pub enum AdminOp {
    LoadStatus,
    LoadPlugins,
    RecoverPlugin { plugin_id: String },
    LoadTrackerProvider,
    SetTrackerProvider { selection: TrackerProviderSelection },
    LoadServices,
    StopService { name: String },
    StartService { name: String },
    // --- merge 审阅流（#25）---
    LoadMergeQueue,
    LoadMergeProposal { proposal_id: String },
    ApproveMerge { proposal_id: String, reviewer: String },
    RejectMerge { proposal_id: String, reviewer: String, reason: Option<String> },
}

impl AdminOp {
    pub fn label(&self) -> &'static str {
        match self {
            AdminOp::LoadStatus => "load admin status",
            AdminOp::LoadPlugins => "load plugins",
            AdminOp::RecoverPlugin { .. } => "recover plugin",
            AdminOp::LoadTrackerProvider => "load tracker provider",
            AdminOp::SetTrackerProvider { .. } => "set tracker provider",
            AdminOp::LoadServices => "load services",
            AdminOp::StopService { .. } => "stop service",
            AdminOp::StartService { .. } => "start service",
            AdminOp::LoadMergeQueue => "load merge review queue",
            AdminOp::LoadMergeProposal { .. } => "load merge proposal",
            AdminOp::ApproveMerge { .. } => "approve merge",
            AdminOp::RejectMerge { .. } => "reject merge",
        }
    }

    /// 是否是写操作（管理面的变更；读失败只影响展示）。
    pub fn is_mutation(&self) -> bool {
        matches!(
            self,
            AdminOp::RecoverPlugin { .. }
                | AdminOp::SetTrackerProvider { .. }
                | AdminOp::StopService { .. }
                | AdminOp::StartService { .. }
                | AdminOp::ApproveMerge { .. }
                | AdminOp::RejectMerge { .. }
        )
    }
}

/// 解码后的管理面响应（与 [`AdminOp`]一一对应）。
#[derive(Clone, Debug, PartialEq)]
pub enum AdminDecoded {
    Status(AdminStatusResponse),
    Plugins(AdminPluginListResponse),
    Recovered(PluginSummary),
    TrackerProvider(TrackerProviderConfig),
    Services(ServiceControlStatus),
    ServiceChanged(ServiceControlStatusEntry),
    MergeQueue(MergeProposalListResponse),
    MergeProposal(MergeProposalDetail),
    Reviewed(MergeReviewResponse),
}

/// 管理面客户端（持有 base url + 管理 token；纯函数拼请求/解响应，无 I/O）。
#[derive(Clone, Debug)]
pub struct AdminClient {
    base_url: String,
    admin_token: String,
}

impl AdminClient {
    pub fn new(base_url: impl Into<String>, admin_token: impl Into<String>) -> Self {
        Self { base_url: base_url.into(), admin_token: admin_token.into() }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 把一次管理面操作拼成待发送的请求（带管理 token + 需要时带 JSON 体）。
    pub fn request_for(&self, op: &AdminOp) -> HttpRequest {
        let headers = vec![
            (ADMIN_TOKEN_HEADER.to_string(), self.admin_token.clone()),
        ];
        let (method, path, body) = match op {
            AdminOp::LoadStatus => ("GET", "/admin/status".to_string(), None),
            AdminOp::LoadPlugins => ("GET", "/admin/plugins".to_string(), None),
            AdminOp::RecoverPlugin { plugin_id } => {
                ("POST", format!("/admin/plugins/{plugin_id}/recover"), None)
            }
            AdminOp::LoadTrackerProvider => ("GET", "/admin/tracker/provider".to_string(), None),
            AdminOp::SetTrackerProvider { selection } => (
                "POST",
                "/admin/tracker/provider".to_string(),
                Some(serde_json::to_string(selection).expect("selection serializes")),
            ),
            AdminOp::LoadServices => ("GET", "/admin/services".to_string(), None),
            AdminOp::StopService { name } => {
                ("POST", format!("/admin/services/{name}/stop"), None)
            }
            AdminOp::StartService { name } => {
                ("POST", format!("/admin/services/{name}/start"), None)
            }
            AdminOp::LoadMergeQueue => ("GET", "/admin/merge/proposals".to_string(), None),
            AdminOp::LoadMergeProposal { proposal_id } => {
                ("GET", format!("/admin/merge/proposals/{proposal_id}"), None)
            }
            AdminOp::ApproveMerge { proposal_id, reviewer } => (
                "POST",
                format!("/admin/merge/proposals/{proposal_id}/approve"),
                Some(review_body(reviewer, None)),
            ),
            AdminOp::RejectMerge { proposal_id, reviewer, reason } => (
                "POST",
                format!("/admin/merge/proposals/{proposal_id}/reject"),
                Some(review_body(reviewer, reason.as_deref())),
            ),
        };
        let mut headers = headers;
        if body.is_some() {
            headers.push(("Content-Type".to_string(), "application/json".to_string()));
        }
        HttpRequest { method, path, query: Vec::new(), headers, body }
    }

    /// 解一个管理面响应（成功 → 对应投影；闭集错误信封 → [`PanelError::Contract`]）。
    pub fn decode(&self, op: &AdminOp, response: HttpResponse) -> Result<AdminDecoded, PanelError> {
        if response.status >= 400 {
            return Err(decode_error(op, &response));
        }
        let decoded = match op {
            AdminOp::LoadStatus => AdminDecoded::Status(parse(op, &response)?),
            AdminOp::LoadPlugins => AdminDecoded::Plugins(parse(op, &response)?),
            AdminOp::RecoverPlugin { .. } => AdminDecoded::Recovered(parse(op, &response)?),
            AdminOp::LoadTrackerProvider => AdminDecoded::TrackerProvider(parse(op, &response)?),
            AdminOp::SetTrackerProvider { .. } => AdminDecoded::TrackerProvider(parse(op, &response)?),
            AdminOp::LoadServices => AdminDecoded::Services(parse(op, &response)?),
            AdminOp::StopService { .. } | AdminOp::StartService { .. } => {
                AdminDecoded::ServiceChanged(parse(op, &response)?)
            }
            AdminOp::LoadMergeQueue => AdminDecoded::MergeQueue(parse(op, &response)?),
            AdminOp::LoadMergeProposal { .. } => AdminDecoded::MergeProposal(parse(op, &response)?),
            AdminOp::ApproveMerge { .. } | AdminOp::RejectMerge { .. } => {
                AdminDecoded::Reviewed(parse(op, &response)?)
            }
        };
        Ok(decoded)
    }
}

/// approve/reject 请求体：`{"reviewer": ..., "reason"?}`（MergeReviewDecision 形状）。
fn review_body(reviewer: &str, reason: Option<&str>) -> String {
    match reason {
        Some(reason) => serde_json::json!({ "reviewer": reviewer, "reason": reason }).to_string(),
        None => serde_json::json!({ "reviewer": reviewer }).to_string(),
    }
}

fn parse<T: serde::de::DeserializeOwned>(op: &AdminOp, response: &HttpResponse) -> Result<T, PanelError> {
    serde_json::from_slice(&response.body).map_err(|e| PanelError::MalformedResponse {
        op: op.label().to_string(),
        status: response.status,
        detail: e.to_string(),
    })
}

fn decode_error(op: &AdminOp, response: &HttpResponse) -> PanelError {
    match serde_json::from_slice::<ContractError>(&response.body) {
        Ok(err) => PanelError::Contract(err),
        Err(e) => PanelError::MalformedResponse {
            op: op.label().to_string(),
            status: response.status,
            detail: e.to_string(),
        },
    }
}
