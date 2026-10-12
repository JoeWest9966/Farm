//! 契约闭集错误码：闭集清单、HTTP/WS 统一形状、缺 token 与错 token 同码。

use farm_sdk::{ContractError, ErrorCode};

/// 独立已知清单（来自契约文档，不重算实现）：闭集一旦增删即失败。
#[test]
fn error_code_closed_set_is_exactly_the_contract_documented_set() {
    let expected = [
        "unauthorized",
        "forbidden",
        "contract_version_unsupported",
        "invalid_request",
        "not_found",
        "method_not_allowed",
        "conflict",
        "not_implemented",
        "service_unavailable",
        "internal",
    ];
    let actual: Vec<&str> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(actual, expected, "闭集错误码清单与契约不符");
}

#[test]
fn missing_token_and_wrong_token_yield_the_same_closed_set_error() {
    let missing = ContractError::unauthorized("missing orchestrator op token");
    let wrong = ContractError::unauthorized("orchestrator op token mismatch");

    assert_eq!(missing.code, ErrorCode::Unauthorized);
    assert_eq!(wrong.code, ErrorCode::Unauthorized);
    assert_eq!(missing.http_status(), 401);
    assert_eq!(wrong.http_status(), 401);
    assert!(!missing.retryable);
}

#[test]
fn error_envelope_serializes_to_the_documented_shape() {
    let err = ContractError::not_implemented("/v1/tracker/issues");
    let json = err.to_json();
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");

    assert_eq!(value["code"], "not_implemented");
    assert_eq!(value["retryable"], false);
    assert!(value["message"].as_str().unwrap().contains("/v1/tracker/issues"));
    assert!(value.get("details").is_none(), "details 缺省不出现");
}

#[test]
fn error_envelope_round_trips_from_json_for_the_ws_face() {
    let err = ContractError::unauthorized("ws upgrade without op token");
    let text = err.to_json();
    let parsed = ContractError::from_json(&text).expect("ws frame must parse as the same envelope");
    assert_eq!(parsed, err);
}

#[test]
fn not_implemented_placeholder_maps_to_501_and_is_not_retryable() {
    let err = ContractError::not_implemented("/v1/merge/proposals");
    assert_eq!(err.http_status(), 501);
    assert!(!err.retryable);
}

#[test]
fn service_unavailable_is_the_only_retryable_code() {
    let retryable: Vec<&str> = ErrorCode::ALL
        .iter()
        .filter(|c| c.is_retryable())
        .map(|c| c.as_str())
        .collect();
    assert_eq!(retryable, ["service_unavailable"]);
}

#[test]
fn http_status_mapping_matches_the_contract_table() {
    let table = [
        (ErrorCode::Unauthorized, 401),
        (ErrorCode::Forbidden, 403),
        (ErrorCode::ContractVersionUnsupported, 400),
        (ErrorCode::InvalidRequest, 400),
        (ErrorCode::NotFound, 404),
        (ErrorCode::MethodNotAllowed, 405),
        (ErrorCode::Conflict, 409),
        (ErrorCode::NotImplemented, 501),
        (ErrorCode::ServiceUnavailable, 503),
        (ErrorCode::Internal, 500),
    ];
    for (code, status) in table {
        assert_eq!(code.http_status(), status, "{} 状态码与契约不符", code);
    }
}
