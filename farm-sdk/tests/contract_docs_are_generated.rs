//! 契约文档是**生成物**：`docs/contract/*` 由 farm-sdk 类型单一源渲染，
//! 本测试断言已签入的文件与生成结果逐字节一致——文档不可能与类型漂移。

use farm_sdk::docs::{self, DOCS_DIR};
use farm_sdk::{CONTRACT_ROUTES, ErrorCode, EventKind, ServiceName};

#[test]
fn generated_contract_docs_match_the_checked_in_files() {
    let rendered = docs::render_all();
    assert!(!rendered.is_empty(), "必须至少生成一个文档文件");

    for (name, content) in rendered {
        let path = DOCS_DIR.join(&name);
        let on_disk = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} 缺失（{}）：先跑 cargo run -p farm-sdk --example gen_contract_docs", name, e));
        assert_eq!(
            on_disk, content,
            "{} 是生成物，已与 farm-sdk 类型漂移；重新生成：cargo run -p farm-sdk --example gen_contract_docs",
            name
        );
    }
}

#[test]
fn generated_schema_is_a_single_document_covering_the_whole_contract() {
    let schema: serde_json::Value =
        serde_json::from_str(&docs::render_schema_json()).expect("schema.json must be valid json");

    assert_eq!(schema["$schema"], "https://json-schema.org/draft/2020-12/schema");
    assert_eq!(schema["x-farm-generated"], true, "生成物必须带标记");

    // 五服务 + 握手 + 闭集都在同一份文档里
    let props = schema["properties"].as_object().expect("properties");
    for key in ["handshake", "routes", "adminRoutes", "errorCodes", "eventKinds", "services"] {
        assert!(props.contains_key(key), "schema 缺 {key}");
    }

    // $defs 覆盖所有契约类型（单一源）
    let defs = schema["$defs"].as_object().expect("$defs");
    for name in [
        "HandshakeRequest",
        "HandshakeResponse",
        "ContractError",
        "ErrorCode",
        "EventEnvelope",
        "EventKind",
        "IssueView",
        "ClaimRequest",
        "DispatchRequest",
        "MergeProposal",
        "WorkspaceMapping",
        "EventHistoryQuery",
        "PluginSummary",
        "AdminStatusResponse",
    ] {
        assert!(defs.contains_key(name), "$defs 缺 {name}");
    }
}

#[test]
fn generated_readme_documents_every_route_and_both_closed_sets() {
    let readme = docs::render_readme_md();

    for route in CONTRACT_ROUTES {
        assert!(readme.contains(route.path), "README 缺路由 {}", route.path);
    }
    for route in farm_sdk::ADMIN_ROUTES {
        assert!(readme.contains(route.path), "README 缺管理面路由 {}", route.path);
    }
    for code in ErrorCode::ALL {
        assert!(readme.contains(code.as_str()), "README 缺错误码 {}", code);
    }
    for kind in EventKind::ALL {
        assert!(readme.contains(kind.as_str()), "README 缺事件种类 {}", kind);
    }
    for service in ServiceName::ALL {
        assert!(readme.contains(service.as_str()), "README 缺服务 {}", service);
    }
    assert!(readme.contains("GENERATED"), "README 必须声明是生成物");
}
