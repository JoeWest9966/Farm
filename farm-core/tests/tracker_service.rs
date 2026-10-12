//! Tracker 服务 v1 的公共接缝测试（ticket #13）。
//!
//! GLOSSARY：Tracker 服务 = 归一化 Issue 模型 + 原生 blocking 边 + 原子 claim
//! （CAS 语义，assignee 即 claim），把 provider 语义关进 adapter。面板与编排器
//! 读同一个归一化视图——tracker 是总线。
//!
//! 接缝选在**服务面**（`TrackerService`）与宿主路由（farm-host 集成测试），
//! 不碰 SQLite 内部：schema 是宿主私有实现细节（ADR-0005 §5）。

use std::sync::Arc;

use farm_core::state::AppState;
use farm_core::tracker::{
    IssueState, NativeAdapter, NativeTicket, TrackerAdapter, TrackerService,
};
use farm_core::{HostConfig};

fn config() -> HostConfig {
    HostConfig {
        bind: "127.0.0.1:3721".parse().expect("valid bind"),
        op_token: "tracker-test-token".into(),
        admin_token: None,
        db_path: std::env::temp_dir().join("farm-tracker-test.db"),
    }
}

fn state_with(adapter: Arc<dyn TrackerAdapter>) -> AppState {
    let mut state = AppState::new_in_memory(config()).expect("in-memory state");
    state.tracker = TrackerService::new(state.db.clone(), adapter);
    state
}

fn native(tickets: Vec<NativeTicket>) -> Arc<dyn TrackerAdapter> {
    Arc::new(NativeAdapter::from_tickets(tickets))
}

/// 三张原生票：#1、#2 是 #13 的 blocker（provider 原生依赖边）。
fn three_tickets() -> Vec<NativeTicket> {
    vec![
        NativeTicket {
            number: 1,
            title: "host skeleton".into(),
            ..Default::default()
        },
        NativeTicket {
            number: 2,
            title: "workflow config".into(),
            ..Default::default()
        },
        NativeTicket {
            number: 13,
            title: "tracker service".into(),
            labels: vec!["tracker".into()],
            blocked_by: vec![1, 2],
            ..Default::default()
        },
    ]
}

#[test]
fn the_normalized_view_exposes_native_blocking_edges() {
    let state = state_with(native(three_tickets()));

    let issues = state.tracker.issues().expect("normalized view");

    let find = |id: &str| {
        issues
            .iter()
            .find(|i| i.issue_id == id)
            .unwrap_or_else(|| panic!("归一化视图必须包含票 {id}"))
            .clone()
    };

    let ticket = find("13");
    assert_eq!(ticket.title, "tracker service");
    assert_eq!(ticket.labels, vec!["tracker".to_string()]);
    assert_eq!(ticket.state, IssueState::Open);
    assert_eq!(ticket.assignee, None, "未被 claim 的票没有 assignee");

    // 边是双向可读的：声明方读 blocked_by，被堵方读 blocking。
    assert_eq!(ticket.blocked_by, vec!["1".to_string(), "2".to_string()]);
    assert_eq!(find("1").blocking, vec!["13".to_string()]);
    assert_eq!(find("2").blocking, vec!["13".to_string()]);
    assert!(find("1").blocked_by.is_empty(), "无依赖的票 blocked_by 为空");
}

// ---------------------------------------------------------------------------
// 原子 claim（CAS 语义，assignee 即 claim）
// ---------------------------------------------------------------------------

#[test]
fn a_claim_takes_exclusive_ownership_of_the_ticket() {
    let state = state_with(native(three_tickets()));

    let claimed = state
        .tracker
        .claim("13", "orchestrator-a")
        .expect("未被持有的票必须能 claim");

    assert_eq!(claimed.issue_id, "13");
    assert_eq!(claimed.assignee.as_deref(), Some("orchestrator-a"));
    assert_eq!(claimed.state, IssueState::Claimed, "assignee 即 claim");

    // 归一化视图与 claim 之后一致（面板与编排器读同一个视图）。
    let view = state.tracker.issues().expect("normalized view");
    let ticket = view
        .iter()
        .find(|i| i.issue_id == "13")
        .expect("票 13 仍在视图里");
    assert_eq!(ticket.assignee.as_deref(), Some("orchestrator-a"));
    assert_eq!(ticket.state, IssueState::Claimed);
}

#[test]
fn a_second_claimant_loses_the_cas_and_is_told_who_holds_the_ticket() {
    let state = state_with(native(three_tickets()));
    state.tracker.claim("13", "orchestrator-a").expect("first claim");

    let err = state
        .tracker
        .claim("13", "orchestrator-b")
        .expect_err("GLOSSARY：失败即已被他人抢走");

    match &err {
        farm_core::TrackerError::Conflict { issue_id, holder } => {
            assert_eq!(issue_id, "13");
            assert_eq!(holder, "orchestrator-a", "冲突要指名当前持有者");
        }
        other => panic!("他人持有的票必须给出 Conflict，收到 {other:?}"),
    }
    // 冲突投影到闭集错误码：409 conflict，details 带当前持有者。
    let contract = err.to_contract_error();
    assert_eq!(contract.code, farm_sdk::ErrorCode::Conflict);
    assert_eq!(contract.http_status(), 409);
    assert_eq!(contract.details.expect("details")["holder"], "orchestrator-a");
}

#[test]
fn claiming_the_same_ticket_twice_with_the_same_assignee_is_idempotent() {
    let state = state_with(native(three_tickets()));
    let first = state.tracker.claim("13", "orchestrator-a").expect("first");
    let second = state.tracker.claim("13", "orchestrator-a").expect("second");

    assert_eq!(first, second, "同一 assignee 重复 claim 是幂等的");
    assert_eq!(second.assignee.as_deref(), Some("orchestrator-a"));
    // 幂等重claim 不写新历史：只有进场 + 第一次 claim 两条。
    let history = state.tracker.history("13").expect("history");
    assert_eq!(history.len(), 2, "幂等重claim 不得造历史：{history:?}");
}

#[test]
fn a_ticket_already_assigned_in_the_provider_is_claimed_by_that_assignee() {
    // provider 语义关进 adapter：GitHub 侧已 assign 的票，归一化状态就是 claimed。
    let tickets = vec![NativeTicket {
        number: 7,
        title: "already assigned".into(),
        assignee: Some("orchestrator-a".into()),
        ..Default::default()
    }];
    let state = state_with(native(tickets));

    let err = state
        .tracker
        .claim("7", "orchestrator-b")
        .expect_err("provider 侧已指派的票不得被他人 claim");

    assert!(
        matches!(&err, farm_core::TrackerError::Conflict { holder, .. } if holder == "orchestrator-a"),
        "收到 {err:?}"
    );
}

#[test]
fn claiming_a_ticket_outside_the_normalized_view_is_not_found() {
    let state = state_with(native(three_tickets()));

    let err = state
        .tracker
        .claim("404", "orchestrator-a")
        .expect_err("provider 没有的票无法 claim");

    assert!(
        matches!(&err, farm_core::TrackerError::NotFound { issue_id } if issue_id == "404"),
        "收到 {err:?}"
    );
    assert_eq!(err.to_contract_error().code, farm_sdk::ErrorCode::NotFound);
}

// ---------------------------------------------------------------------------
// release：幂等 + release 后他人可 claim
// ---------------------------------------------------------------------------

#[test]
fn after_a_release_another_assignee_can_claim() {
    let state = state_with(native(three_tickets()));
    state.tracker.claim("13", "orchestrator-a").expect("first claim");

    let outcome = state
        .tracker
        .release("13", "orchestrator-a")
        .expect("持有者本人必须能释放");
    assert_eq!(outcome, farm_core::tracker::ReleaseOutcome::Released);

    let reclaimed = state
        .tracker
        .claim("13", "orchestrator-b")
        .expect("release 之后他人必须能 claim");
    assert_eq!(reclaimed.assignee.as_deref(), Some("orchestrator-b"));
    assert_eq!(reclaimed.state, IssueState::Claimed);
}

#[test]
fn releasing_a_ticket_nobody_holds_is_an_idempotent_no_op() {
    let state = state_with(native(three_tickets()));

    let outcome = state
        .tracker
        .release("13", "orchestrator-a")
        .expect("无人持有的 release 是幂等空操作，不是错误");
    assert_eq!(outcome, farm_core::tracker::ReleaseOutcome::NotClaimed);

    // 空操作之后票仍是未持有的 open，别人照样能 claim。
    let claimed = state.tracker.claim("13", "orchestrator-b").expect("claim");
    assert_eq!(claimed.state, IssueState::Claimed);
}

#[test]
fn releasing_twice_by_the_same_assignee_is_idempotent() {
    let state = state_with(native(three_tickets()));
    state.tracker.claim("13", "orchestrator-a").expect("claim");

    let first = state.tracker.release("13", "orchestrator-a").expect("first release");
    let second = state.tracker.release("13", "orchestrator-a").expect("second release");

    assert_eq!(first, farm_core::tracker::ReleaseOutcome::Released);
    assert_eq!(second, farm_core::tracker::ReleaseOutcome::NotClaimed);
    assert_eq!(
        state.tracker.issues().expect("view")[2].assignee, None,
        "重复 release 之后票必须回到无人持有"
    );
}

#[test]
fn a_non_holder_cannot_release_someone_elses_claim() {
    let state = state_with(native(three_tickets()));
    state.tracker.claim("13", "orchestrator-a").expect("claim");

    let err = state
        .tracker
        .release("13", "orchestrator-b")
        .expect_err("非持有者不得释放他人的 claim");

    assert!(
        matches!(&err, farm_core::TrackerError::Conflict { holder, .. } if holder == "orchestrator-a"),
        "收到 {err:?}"
    );
    assert_eq!(
        state.tracker.claim("13", "orchestrator-a").expect("idempotent re-claim").assignee.as_deref(),
        Some("orchestrator-a"),
        "失败的 release 不得动到原持有者"
    );
}

#[test]
fn releasing_a_ticket_outside_the_normalized_view_is_not_found() {
    let state = state_with(native(three_tickets()));

    let err = state
        .tracker
        .release("404", "orchestrator-a")
        .expect_err("provider 没有的票无法 release");

    assert!(
        matches!(&err, farm_core::TrackerError::NotFound { issue_id } if issue_id == "404"),
        "收到 {err:?}"
    );
}

// ---------------------------------------------------------------------------
// 票状态历史：落库 + 可查询（ADR-0005 §2 持久项）
// ---------------------------------------------------------------------------

fn file_state(path: &std::path::Path, adapter: Arc<dyn TrackerAdapter>) -> AppState {
    let config = HostConfig {
        bind: "127.0.0.1:3721".parse().expect("valid bind"),
        op_token: "tracker-test-token".into(),
        admin_token: None,
        db_path: path.to_path_buf(),
    };
    let mut state = AppState::new(config).expect("open host-private store");
    state.tracker = TrackerService::new(state.db.clone(), adapter);
    state
}

#[test]
fn ticket_state_history_is_persisted_and_queryable_after_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("farm.db");

    {
        let state = file_state(&path, native(three_tickets()));
        state.tracker.claim("13", "orchestrator-a").expect("claim");
        state.tracker.release("13", "orchestrator-a").expect("release");
    }

    // 重启：同一个库、默认（空）native provider——历史不依赖 adapter 就能回读。
    let reopened = AppState::new(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("valid bind"),
        op_token: "tracker-test-token".into(),
        admin_token: None,
        db_path: path.clone(),
    })
    .expect("reopen host-private store");

    let history = reopened.tracker.history("13").expect("state history");
    let states: Vec<(Option<IssueState>, IssueState)> = history
        .iter()
        .map(|t| (t.from_state, t.to_state))
        .collect();
    assert_eq!(
        states,
        vec![
            (None, IssueState::Open),        // provider sync：票进归一化视图
            (Some(IssueState::Open), IssueState::Claimed), // 原子 claim
            (Some(IssueState::Claimed), IssueState::Open), // release
        ],
        "状态历史必须按次序记全每次变迁：{history:?}"
    );
    assert_eq!(history[1].assignee.as_deref(), Some("orchestrator-a"));
    assert_eq!(history[1].actor, "orchestrator-a", "claim 的 actor 就是 claimant");
    assert_eq!(history[2].assignee, None, "release 之后无人持有");
    assert!(!history[0].occurred_at.is_empty(), "每次变迁都盖时间戳");
}

#[test]
fn a_sync_that_changes_nothing_fabricates_no_history() {
    let state = state_with(native(three_tickets()));
    state.tracker.issues().expect("first read");

    let report = state.tracker.sync().expect("second sync");

    assert_eq!(report.provider, "native");
    assert_eq!(report.fetched, 3);
    assert_eq!(report.changed, 0, "无变化的 sync 不得写库");
    assert_eq!(
        state.tracker.history_all().expect("history").len(),
        3,
        "只有三张票进场的三条历史"
    );
}

#[test]
fn a_provider_state_change_is_recorded_in_the_state_history() {
    let mut state = state_with(native(three_tickets()));
    state.tracker.issues().expect("first read");

    // provider 侧把 #13 关了：换一个 adapter（同一个库）模拟 provider 事实变化。
    let closed = vec![NativeTicket {
        number: 13,
        title: "tracker service".into(),
        state: farm_core::tracker::NativeTicketState::Closed,
        blocked_by: vec![1, 2],
        ..Default::default()
    }];
    state.tracker = TrackerService::new(state.db.clone(), native(closed));
    let view = state.tracker.issues().expect("second read");

    let ticket = view.iter().find(|i| i.issue_id == "13").expect("票 13");
    assert_eq!(ticket.state, IssueState::Done, "provider 的关闭是不可逆事实");

    let history = state.tracker.history("13").expect("history");
    assert_eq!(history.len(), 2, "进场 + 一次状态变迁");
    assert_eq!(history[1].from_state, Some(IssueState::Open));
    assert_eq!(history[1].to_state, IssueState::Done);
    assert_eq!(history[1].actor, "native", "provider sync 的 actor 是 provider 名");
    assert!(history[1].note.contains("provider sync"), "{}", history[1].note);
}

#[test]
fn a_sync_does_not_silently_drop_a_farm_claim() {
    let mut state = state_with(native(three_tickets()));
    state.tracker.claim("13", "orchestrator-a").expect("claim");

    // provider 仍报未指派：claim 是 Farm 侧的排他声明，sync 不得把它抹掉。
    state.tracker = TrackerService::new(state.db.clone(), native(three_tickets()));
    let view = state.tracker.issues().expect("view after sync");

    let ticket = view.iter().find(|i| i.issue_id == "13").expect("票 13");
    assert_eq!(ticket.assignee.as_deref(), Some("orchestrator-a"));
    assert_eq!(ticket.state, IssueState::Claimed);
}

#[test]
fn a_provider_reassignment_overrides_a_farm_claim_and_is_recorded() {
    let mut state = state_with(native(three_tickets()));
    state.tracker.claim("13", "orchestrator-a").expect("claim");

    // provider 改派给别人：provider 是上游事实，覆盖要留痕。
    let reassigned = vec![NativeTicket {
        number: 13,
        title: "tracker service".into(),
        assignee: Some("human-bob".into()),
        ..Default::default()
    }];
    state.tracker = TrackerService::new(state.db.clone(), native(reassigned));
    let view = state.tracker.issues().expect("view after sync");

    let ticket = view.iter().find(|i| i.issue_id == "13").expect("票 13");
    assert_eq!(ticket.assignee.as_deref(), Some("human-bob"));

    let history = state.tracker.history("13").expect("history");
    let last = history.last().expect("at least one transition");
    assert_eq!(last.to_state, IssueState::Claimed);
    assert_eq!(last.assignee.as_deref(), Some("human-bob"));
}

// ---------------------------------------------------------------------------
// 按五服务分域的 migration 组织（#13 建立的先例）
// ---------------------------------------------------------------------------

#[test]
fn the_tracker_service_domain_migration_lands_its_own_tables() {
    let mut state = AppState::new_in_memory(config()).expect("state");

    let versions: Vec<(u32, &str)> = farm_core::MIGRATIONS
        .iter()
        .map(|m| (m.version, m.name))
        .collect();
    assert_eq!(versions[0], (1, "host_meta"), "v1 是 #11 的宿主级表，不得改动");
    assert!(
        versions.contains(&(3, "tracker_issue_snapshot")),
        "tracker 服务域必须独占一个版本号：{versions:?}"
    );
    let applied = state.db.applied_migrations().expect("applied list");
    assert!(
        applied.iter().any(|m| m.version == 3 && m.name == "tracker_issue_snapshot"),
        "v3 必须已落地：{applied:?}"
    );

    // 迁移落地后服务立即可用：票快照/边/历史三张表由 tracker 域自己的 migration 建。
    state.tracker = TrackerService::new(state.db.clone(), native(three_tickets()));
    assert_eq!(state.tracker.issues().expect("normalized view").len(), 3);
    state.tracker.claim("13", "orchestrator-a").expect("claim");
    assert_eq!(state.tracker.history("13").expect("history").len(), 2);
}
