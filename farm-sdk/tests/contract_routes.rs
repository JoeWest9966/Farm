//! 契约路由清单：路径单一源，宿主/文档/客户端三方共用。

use farm_sdk::routes::{contract_path, CONTRACT_ROUTES};
use farm_sdk::services::ServiceName;
use farm_sdk::{ADMIN_PREFIX, ADMIN_ROUTES, CONTRACT_PREFIX, HANDSHAKE_PATH};

#[test]
fn the_five_services_map_to_the_five_contract_prefixes() {
    let segments: Vec<&str> = ServiceName::ALL.iter().map(|s| s.path_segment()).collect();
    assert_eq!(
        segments,
        ["tracker", "agent-runner", "merge", "workspace", "events"]
    );
    for service in ServiceName::ALL {
        assert_eq!(contract_path(service, ""), format!("{}/{}", CONTRACT_PREFIX, service));
    }
}

#[test]
fn every_contract_route_lives_under_its_service_prefix() {
    for route in CONTRACT_ROUTES {
        assert!(
            route.path.starts_with(&format!("{}/{}/", CONTRACT_PREFIX, route.service.path_segment())),
            "{} 不在 {} 服务前缀下",
            route.path,
            route.service
        );
        assert!(
            route.method == "GET" || route.method == "POST",
            "占位路由只用到 GET/POST，收到 {}",
            route.method
        );
        assert!(!route.summary.is_empty(), "每条路由都要有一句话说明（进生成文档）");
    }
}

#[test]
fn the_skeleton_reserves_one_route_per_service() {
    let covered: Vec<ServiceName> = ServiceName::ALL
        .iter()
        .copied()
        .filter(|s| CONTRACT_ROUTES.iter().any(|r| r.service == *s))
        .collect();
    assert_eq!(covered, ServiceName::ALL.to_vec(), "五服务都必须有占位路由");
}

#[test]
fn the_ws_event_stream_is_a_contract_route() {
    let ws = CONTRACT_ROUTES
        .iter()
        .find(|r| r.path == "/v1/events/stream")
        .expect("事件总线必须有 WS 路由");
    assert_eq!(ws.method, "GET");
    assert_eq!(ws.service, ServiceName::Events);
}

#[test]
fn handshake_route_is_the_only_route_outside_the_contract_prefix() {
    assert_eq!(HANDSHAKE_PATH, "/handshake");
    assert!(!HANDSHAKE_PATH.starts_with(CONTRACT_PREFIX));
    for route in CONTRACT_ROUTES {
        assert_ne!(route.path, HANDSHAKE_PATH);
    }
}

#[test]
fn admin_routes_are_a_separate_prefix_from_the_contract() {
    assert_eq!(ADMIN_PREFIX, "/admin");
    assert!(!ADMIN_ROUTES.is_empty(), "宿主管理 API 必须留接口位");
    for route in ADMIN_ROUTES {
        assert!(route.path.starts_with(ADMIN_PREFIX), "{} 不在管理面前缀下", route.path);
        for contract in CONTRACT_ROUTES {
            assert_ne!(
                route.path, contract.path,
                "管理面路由不得与契约面路由重合"
            );
        }
    }
}
