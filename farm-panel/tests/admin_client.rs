//! 管理面客户端接缝测试（ticket #25）：请求形状 + 管理 token + 「只走 /admin」不变式 + 解码。

use farm_panel::core::admin::{AdminClient, AdminOp, ADMIN_TOKEN_HEADER};
use farm_panel::core::client::{HttpResponse, PanelError};
use farm_sdk::services::merge::MergeProposalListResponse;

fn client() -> AdminClient {
    AdminClient::new("http://127.0.0.1:3721", "admin-secret")
}

#[test]
fn every_admin_op_targets_the_admin_prefix_with_the_admin_token() {
    let c = client();
    let ops = vec![
        AdminOp::LoadStatus,
        AdminOp::LoadPlugins,
        AdminOp::RecoverPlugin { plugin_id: "p1".into() },
        AdminOp::LoadTrackerProvider,
        AdminOp::LoadServices,
        AdminOp::StopService { name: "merge".into() },
        AdminOp::StartService { name: "merge".into() },
        AdminOp::LoadMergeQueue,
        AdminOp::LoadMergeProposal { proposal_id: "mrg-1".into() },
        AdminOp::ApproveMerge { proposal_id: "mrg-1".into(), reviewer: "joe".into() },
        AdminOp::RejectMerge { proposal_id: "mrg-1".into(), reviewer: "joe".into(), reason: Some("no".into()) },
    ];
    for op in ops {
        let req = c.request_for(&op);
        assert!(req.path.starts_with("/admin"), "{} 必须走 /admin：{}", op.label(), req.path);
        assert!(
            req.headers.iter().any(|(k, v)| k == ADMIN_TOKEN_HEADER && v == "admin-secret"),
            "{} 必须带管理 token", op.label()
        );
        // URL 拼出 base + 管理面前缀。
        assert!(req.url("http://127.0.0.1:3721").starts_with("http://127.0.0.1:3721/admin"));
    }
}

#[test]
fn approving_and_rejecting_build_the_review_decision_bodies() {
    let c = client();
    let approve = c.request_for(&AdminOp::ApproveMerge { proposal_id: "mrg-1".into(), reviewer: "joe".into() });
    assert_eq!(approve.method, "POST");
    assert_eq!(approve.path, "/admin/merge/proposals/mrg-1/approve");
    let body: serde_json::Value = serde_json::from_str(approve.body.as_deref().expect("body")).expect("json");
    assert_eq!(body["reviewer"], "joe");
    assert!(body.get("reason").is_none(), "approve 不带 reason");

    let reject = c.request_for(&AdminOp::RejectMerge { proposal_id: "mrg-1".into(), reviewer: "joe".into(), reason: Some("needs tests".into()) });
    assert_eq!(reject.path, "/admin/merge/proposals/mrg-1/reject");
    let body: serde_json::Value = serde_json::from_str(reject.body.as_deref().expect("body")).expect("json");
    assert_eq!(body["reviewer"], "joe");
    assert_eq!(body["reason"], "needs tests");
}

#[test]
fn the_merge_queue_decodes_and_a_closed_set_error_surfaces_as_a_contract_error() {
    let c = client();
    let op = AdminOp::LoadMergeQueue;

    // 成功：审阅队列解码。
    let queue = MergeProposalListResponse { proposals: vec![] };
    let resp = HttpResponse { status: 200, body: serde_json::to_vec(&queue).expect("ser") };
    match c.decode(&op, resp).expect("decode") {
        farm_panel::core::admin::AdminDecoded::MergeQueue(q) => assert!(q.proposals.is_empty()),
        other => panic!("wrong decode: {other:?}"),
    }

    // 闭集错误信封 → PanelError::Contract（面板据闭集码渲染，不猜）。
    let err = farm_sdk::ContractError::forbidden("admin token required");
    let resp = HttpResponse { status: 403, body: err.to_json().into_bytes() };
    match c.decode(&op, resp).expect_err("403 must error") {
        PanelError::Contract(e) => assert_eq!(e.code, farm_sdk::ErrorCode::Forbidden),
        other => panic!("wrong error: {other:?}"),
    }
}
