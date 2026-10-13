//! Merge 审阅契约形状（ticket #25）：审阅决策 / 提议详情 / 审阅队列 / 审阅结果。

use farm_sdk::services::merge::{
    MergeProposalDetail, MergeProposalListResponse, MergeProposalState, MergeReviewDecision,
    MergeReviewResponse,
};

#[test]
fn a_review_decision_round_trips_and_rejects_unknown_fields() {
    let approve = MergeReviewDecision { reviewer: "joe".into(), reason: None };
    let json = serde_json::to_string(&approve).expect("serialize");
    assert!(!json.contains("reason"), "approve 省略 reason");
    let back: MergeReviewDecision = serde_json::from_str(&json).expect("parse");
    assert_eq!(back, approve);

    let reject = MergeReviewDecision { reviewer: "joe".into(), reason: Some("not ready".into()) };
    let back: MergeReviewDecision =
        serde_json::from_str(&serde_json::to_string(&reject).expect("ser")).expect("parse");
    assert_eq!(back.reason.as_deref(), Some("not ready"));

    // 前向兼容的另一面：未知字段拒绝（契约闭集，写错字段名即非法）。
    let err = serde_json::from_str::<MergeReviewDecision>(r#"{"reviewer":"joe","nope":1}"#);
    assert!(err.is_err(), "未知字段必须拒绝");
}

#[test]
fn a_proposal_detail_carries_the_diff_and_the_completion_signal() {
    let merged = MergeProposalDetail {
        proposal_id: "mrg-1".into(),
        ticket_id: "25".into(),
        worktree_branch: "farm/t25".into(),
        integration_branch: "implement/spec-10".into(),
        state: MergeProposalState::Merged,
        message: "reviewed".into(),
        merge_commit: Some("abc123".into()),
        rejection_stage: None,
        rejection_detail: None,
        diff: "diff --git a/feature.txt ...".into(),
    };
    let back: MergeProposalDetail =
        serde_json::from_str(&serde_json::to_string(&merged).expect("ser")).expect("parse");
    assert_eq!(back, merged);
    assert_eq!(back.state, MergeProposalState::Merged);
    assert_eq!(back.merge_commit.as_deref(), Some("abc123"), "完成信号在详情里");
    assert!(back.diff.contains("feature.txt"), "diff 供面板呈现");

    let queue = MergeProposalListResponse { proposals: vec![merged.clone()] };
    let back: MergeProposalListResponse =
        serde_json::from_str(&serde_json::to_string(&queue).expect("ser")).expect("parse");
    assert_eq!(back.proposals.len(), 1);
}

#[test]
fn a_review_response_reports_the_state_and_optional_completion_signal() {
    let approved = MergeReviewResponse {
        proposal_id: "mrg-1".into(),
        state: MergeProposalState::Merged,
        merge_commit: Some("abc123".into()),
    };
    let rejected = MergeReviewResponse {
        proposal_id: "mrg-2".into(),
        state: MergeProposalState::Rejected,
        merge_commit: None,
    };
    for r in [approved, rejected] {
        let back: MergeReviewResponse =
            serde_json::from_str(&serde_json::to_string(&r).expect("ser")).expect("parse");
        assert_eq!(back, r);
    }
}
