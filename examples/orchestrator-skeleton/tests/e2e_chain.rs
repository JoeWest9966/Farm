//! 端到端整链集成测试（ticket #26）：对**真实 farm-host 二进制**跑通整链 +
//! 崩溃恢复演练，每步机械断言。找不到 fake 替身 / notifier / 宿主二进制就跳过
//! （CI 先编 farm-host + fake 替身 + http-notifier 再跑本测试）。

use orchestrator_skeleton::harness;
use orchestrator_skeleton::{run_full_chain, ChainBinaries};

#[tokio::test]
async fn the_full_chain_runs_green_with_the_crash_recovery_drill() {
    let (fake_pi, fake_nw, fake_ledger, notifier) = match (
        harness::fake_bin("fake-pi"),
        harness::fake_bin("fake-nw"),
        harness::fake_bin("fake-ledger"),
        harness::notifier_bin(),
    ) {
        (Some(a), Some(b), Some(c), Some(d)) => (a, b, c, d),
        _ => {
            eprintln!(
                "fake binaries (fake-pi/fake-nw/fake-ledger) or http-notifier not found; \
                 skipping the e2e chain (build farm-host + fakes + http-notifier first)"
            );
            return;
        }
    };
    if harness::host_bin().is_none() {
        eprintln!("farm-host binary not found; skipping (build it first)");
        return;
    }

    let binaries = ChainBinaries { fake_pi, fake_nw, fake_ledger, notifier };
    let report = run_full_chain(&binaries).await;

    assert!(
        report.all_ok(),
        "the end-to-end chain did not run green:\n{}",
        report.render()
    );

    // 防空洞：整链至少覆盖一批机械断言。
    assert!(
        report.checks.len() >= 18,
        "the e2e chain should assert each step (>= 18 checks), got {}",
        report.checks.len()
    );
}
