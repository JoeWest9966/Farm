//! 闭集错误码（closed-set error codes）——契约唯一错误面。
//!
//! 契约零域知识（ADR-0001 决策 4/6）：错误码是一个**闭集**，编排器与面板可以
//! 穷举它；新增码是契约变更，不是实现细节。HTTP 与 WS 两个面共用同一形状。

use serde::{Deserialize, Serialize};

/// 契约错误码闭集。
///
/// 字符串形式即编排器看到的 `code` 字段（snake_case）。`ALL` 是唯一权威清单，
/// 一致性套件与生成文档都以它为准。
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 无 token 或 token 错误。缺 token 与错 token **同码**（不做区分，避免预言机）。
    Unauthorized,
    /// 已鉴权但无权限（例如把操作 token 当管理 token 用）。
    Forbidden,
    /// 握手时契约版本不被宿主支持。
    ContractVersionUnsupported,
    /// 请求体/参数不符合契约 schema。
    InvalidRequest,
    /// 路由不存在。
    NotFound,
    /// 路由存在但方法不对。
    MethodNotAllowed,
    /// 状态冲突（例如原子 claim 已被他人抢走）。
    Conflict,
    /// 路由已占位、服务未实现（本票骨架的统一回答）。
    NotImplemented,
    /// 宿主暂时不可用（可重试）。
    ServiceUnavailable,
    /// 宿主内部错误（不可重试）。
    Internal,
}

impl ErrorCode {
    /// 闭集的权威清单。测试与生成文档都以此为准。
    pub const ALL: [ErrorCode; 10] = [
        ErrorCode::Unauthorized,
        ErrorCode::Forbidden,
        ErrorCode::ContractVersionUnsupported,
        ErrorCode::InvalidRequest,
        ErrorCode::NotFound,
        ErrorCode::MethodNotAllowed,
        ErrorCode::Conflict,
        ErrorCode::NotImplemented,
        ErrorCode::ServiceUnavailable,
        ErrorCode::Internal,
    ];

    /// 契约里出现的字符串形式。
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::ContractVersionUnsupported => "contract_version_unsupported",
            ErrorCode::InvalidRequest => "invalid_request",
            ErrorCode::NotFound => "not_found",
            ErrorCode::MethodNotAllowed => "method_not_allowed",
            ErrorCode::Conflict => "conflict",
            ErrorCode::NotImplemented => "not_implemented",
            ErrorCode::ServiceUnavailable => "service_unavailable",
            ErrorCode::Internal => "internal",
        }
    }

    /// 该码对应的 HTTP 状态码。WS 面被拒时同样返回这个状态 + 同一 JSON 体。
    pub fn http_status(&self) -> u16 {
        match self {
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

    /// 客户端是否可以安全重试。
    pub fn is_retryable(&self) -> bool {
        matches!(self, ErrorCode::ServiceUnavailable)
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 契约错误信封：HTTP 响应体与 WS 文本帧共用同一形状。
///
/// ```json
/// {"code":"not_implemented","message":"...","retryable":false}
/// ```
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContractError {
    pub code: ErrorCode,
    pub message: String,
    /// 可选的附加信息（开放形状）；闭集只在 `code` 上。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    /// 是否可重试；与 `code.is_retryable()` 一致。
    #[serde(default)]
    pub retryable: bool,
}

impl ContractError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
            retryable: code.is_retryable(),
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    /// 缺 token / 错 token 的统一回答（同码同状态）。
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthorized, message)
    }

    /// 宿主管理 API 的独立鉴权失败同样走这个码，但消息指明是管理 token。
    pub fn admin_unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthorized, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Forbidden, message)
    }

    pub fn contract_version_unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ContractVersionUnsupported, message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn method_not_allowed(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::MethodNotAllowed, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    /// 占位路由的统一回答：路由已占位、服务未实现。
    pub fn not_implemented(route: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::NotImplemented,
            format!("route {} is reserved by the contract skeleton; service not implemented yet", route.into()),
        )
    }

    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ServiceUnavailable, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn http_status(&self) -> u16 {
        self.code.http_status()
    }

    /// 序列化为 JSON 文本（WS 帧与 HTTP 体一致）。
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            serde_json::to_string(&ContractError::internal("error envelope is not serializable"))
                .expect("internal error envelope must serialize")
        })
    }

    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ContractError {}

impl From<serde_json::Error> for ContractError {
    fn from(e: serde_json::Error) -> Self {
        ContractError::invalid_request(format!("malformed json: {e}"))
    }
}
