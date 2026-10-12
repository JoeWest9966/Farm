//! 一致性套件集成测试（ticket #26）：对**真实 farm-host 二进制**跑「符合契约」凭证。
//!
//! 定位并 spawn 真宿主（含 admin token + 一张播种票），以契约客户端身份跑
//! [`run_conformance`]，逐条断言契约面全绿。找不到宿主二进制就跳过（CI 先编 farm-host）。

mod common;

use common::TestHost;
use orchestrator_skeleton::run_conformance;

#[tokio::test]
async fn the_conformance_suite_is_green_against_the_real_farm_host_binary() {
    if common::host_bin().is_none() {
        eprintln!("farm-host binary not found; skipping (build it first: cargo build -p farm-host)");
        return;
    }

    let host = TestHost::spawn(&[], true).await;
    let client = host.client();
    let report = run_conformance(&client).await;
    drop(host); // 收尾宿主进程，不残留

    // 全套件必须全绿；失败时打印每一条机械事实便于定位。
    assert!(
        report.all_ok(),
        "conformance suite not green against the real farm-host binary:\n{}",
        report.render()
    );

    // 防「套件悄悄变空洞」：至少覆盖一批断言。
    assert!(
        report.checks.len() >= 20,
        "conformance suite should be comprehensive (>= 20 checks), got {}",
        report.checks.len()
    );
}
