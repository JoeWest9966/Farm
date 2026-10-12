//! 冒烟测试：拉起真实 `farm-host` 二进制，跑契约冒烟，逐项断言。
//!
//! 定位宿主二进制：`FARM_HOST_BIN` 优先，否则 `<workspace>/target/debug/farm-host`。
//! 找不到就跳过（CI 先编 farm-host 再跑本测试）。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use orchestrator_skeleton::run_contract_smoke;
use tokio::net::TcpStream;

fn host_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("FARM_HOST_BIN") {
        return Some(PathBuf::from(p));
    }
    // 尊重 CARGO_TARGET_DIR（本环境全局设在 /opt/Warehousetui/target）。
    if let Ok(td) = std::env::var("CARGO_TARGET_DIR") {
        let cand = PathBuf::from(td).join("debug/farm-host");
        if cand.exists() {
            return Some(cand);
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")); // .../examples/orchestrator-skeleton
    let cand = manifest.join("../../target/debug/farm-host");
    cand.exists().then_some(cand)
}

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn wait_listening(addr: SocketAddr, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(addr).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn the_skeleton_completes_the_handshake_against_a_live_host() {
    let Some(bin) = host_bin() else {
        eprintln!("FARM_HOST_BIN not found; skipping live contract smoke (build farm-host first)");
        return;
    };

    let token = "skeleton-smoke-token";
    let addr: SocketAddr = "127.0.0.1:3799".parse().expect("valid bind");
    let db = std::env::temp_dir().join(format!("farm-skeleton-smoke-{}.db", std::process::id()));

    let host = Host(
        Command::new(&bin)
            .env("FARM_TOKEN", token)
            .env("FARM_BIND", addr.to_string())
            .env("FARM_DB", &db)
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display())),
    );

    assert!(
        wait_listening(addr, Duration::from_secs(30)).await,
        "farm-host did not start listening on {addr}"
    );

    let checks = run_contract_smoke(addr, token).await;
    drop(host); // 停宿主，进程不残留
    let _ = std::fs::remove_file(&db);

    let mut failures = Vec::new();
    for c in &checks {
        if !c.ok {
            failures.push(format!("{}: {}", c.name, c.detail));
        }
    }
    assert!(failures.is_empty(), "contract smoke failures:\n{}", failures.join("\n"));
}
