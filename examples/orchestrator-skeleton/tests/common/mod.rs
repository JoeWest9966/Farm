//! 测试夹具：拉起**真实 farm-host 二进制**（ticket #26）。
//!
//! 一致性套件与整链 tracer 都对**真宿主进程**跑（不是 in-process router）：定位宿主二进制、
//! 选临时回环端口、隔离临时目录（DB / 原生票 / fake 替身日志），按 env 装配后 spawn，
//! 等它 listen。Drop 时收尾（关 stdin 语义不适用于 SIGKILL 场景，故 kill + wait）。

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use orchestrator_skeleton::ContractClient;
use tokio::net::TcpStream;

/// 定位 farm-host 二进制：`FARM_HOST_BIN` 优先，否则 `CARGO_TARGET_DIR/debug/farm-host`，
/// 再否则 `<manifest>/../../target/debug/farm-host`。
pub fn host_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("FARM_HOST_BIN") {
        return Some(PathBuf::from(p));
    }
    if let Ok(td) = std::env::var("CARGO_TARGET_DIR") {
        let cand = PathBuf::from(td).join("debug/farm-host");
        if cand.exists() {
            return Some(cand);
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cand = manifest.join("../../target/debug/farm-host");
    cand.exists().then_some(cand)
}

/// 定位一个 farm-core fake 替身二进制（fake-pi / fake-nw / fake-ledger）。
pub fn fake_bin(name: &str) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        let cand = PathBuf::from(dir).join("debug").join(name);
        if cand.exists() {
            return Some(cand);
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cand = manifest.join("../../target/debug").join(name);
    cand.exists().then_some(cand)
}

/// 定位 http-notifier 插件二进制（examples/http-notifier，独立 workspace 单独编）。
pub fn notifier_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("FARM_NOTIFIER_BIN") {
        return Some(PathBuf::from(p));
    }
    if let Ok(td) = std::env::var("CARGO_TARGET_DIR") {
        // examples/ 各自独立 workspace，产物落在各自 target/；也兼容共享 target。
        for cand in [
            PathBuf::from(&td).join("debug/http-notifier"),
            PathBuf::from(&td).join("examples/http-notifier/debug/http-notifier"),
        ] {
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    None
}

/// 选一个临时回环端口（预绑 :0 拿到 OS 分配的端口后放开，交给宿主 bind）。
/// 窗口极小（回环、CI 单测串行），是测试里取临时端口的常规手法。
pub fn ephemeral_port() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    addr
}

async fn wait_listening(addr: SocketAddr, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(addr).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// 一个被测的真实 farm-host 进程 + 它的临时目录（保持存活）。
pub struct TestHost {
    child: Child,
    pub addr: SocketAddr,
    pub op_token: String,
    pub admin_token: Option<String>,
    pub dir: tempfile::TempDir,
}

impl TestHost {
    /// spawn 宿主；`extra_env` 是追加的环境变量（fake 替身路径 / 场景 / 闸门 / 票源…）。
    /// `with_admin` 决定是否配 admin token（一致性套件要验管理面边界）。
    pub async fn spawn(extra_env: &[(&str, String)], with_admin: bool) -> Self {
        let bin = host_bin().unwrap_or_else(|| panic!("farm-host binary not found; build it first (cargo build -p farm-host)"));
        let dir = tempfile::tempdir().expect("tempdir");
        let addr = ephemeral_port();
        let op_token = "conformance-op-token".to_string();
        let admin_token = with_admin.then(|| "conformance-admin-token".to_string());

        // 播种一张 open 原生票 #26（dispatchable 标签；供 claim/CAS/整链取用）。
        let tickets = dir.path().join("tickets.json");
        std::fs::write(
            &tickets,
            r#"[{"number":26,"title":"capstone tracer ticket","state":"open","labels":["dispatch"],"blocked_by":[]}]"#,
        )
        .expect("write seed tickets");

        let mut command = Command::new(&bin);
        command
            .env("FARM_BIND", addr.to_string())
            .env("FARM_TOKEN", &op_token)
            .env("FARM_DB", dir.path().join("farm.db"))
            .env("FARM_NATIVE_TICKETS", &tickets)
            .env("RUST_LOG", "warn"); // 安静：只留告警，测试断言不靠日志
        if let Some(admin) = &admin_token {
            command.env("FARM_ADMIN_TOKEN", admin);
        }
        for (key, value) in extra_env {
            command.env(key, value);
        }

        let child = command.spawn().unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));
        let host = Self { child, addr, op_token, admin_token, dir };
        assert!(
            wait_listening(addr, Duration::from_secs(30)).await,
            "farm-host did not start listening on {addr}"
        );
        host
    }

    /// 便利构造器（无追加 env、无 admin）。
    pub async fn bare() -> Self {
        Self::spawn(&[], false).await
    }

    pub fn client(&self) -> ContractClient {
        ContractClient::new(self.addr, self.op_token.clone(), self.admin_token.clone())
    }

    pub fn db_path(&self) -> PathBuf {
        // FARM_DB 指向 dir/farm.db（+ WAL/SHM sidecar）。
        self.dir.path().join("farm.db")
    }

    /// SIGKILL：模拟宿主崩溃（不是有序停机）——崩溃恢复演练用。
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
