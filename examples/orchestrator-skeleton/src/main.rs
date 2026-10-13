//! orchestrator-skeleton 二进制：连上宿主、完成 `/handshake`、收到合法闭集错误。
//!
//! 环境变量：`FARM_HOST`（默认 `127.0.0.1:3721`）、`FARM_TOKEN`（操作 token）。

use std::net::SocketAddr;

use orchestrator_skeleton::run_contract_smoke;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let host = std::env::var("FARM_HOST").unwrap_or_else(|_| "127.0.0.1:3721".to_string());
    let addr: SocketAddr = host
        .parse()
        .unwrap_or_else(|e| panic!("FARM_HOST must be host:port, got {host:?}: {e}"));
    let token = std::env::var("FARM_TOKEN").unwrap_or_default();

    println!("orchestrator-skeleton → {addr} (contract client)");
    let checks = run_contract_smoke(addr, &token).await;

    let mut all_ok = true;
    for c in &checks {
        println!("[{}] {} — {}", if c.ok { "PASS" } else { "FAIL" }, c.name, c.detail);
        all_ok &= c.ok;
    }

    if all_ok {
        println!("\ncontract face reachable; closed-set errors confirmed. (Refs #11)");
    } else {
        eprintln!("\ncontract smoke FAILED.");
        std::process::exit(1);
    }
}
