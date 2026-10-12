//! farm-host 二进制入口（ADR-0003 单二进制）：装配路由、绑定回环、伺服三面。
//!
//! 与测试共用同一个装配点 [`build_router`]，所以真实跑起来的伺服面与测试断言
//! 的面是同构的。默认绑 `127.0.0.1`（spec 安全裁决：Farm 永不自带公网暴露）。

use farm_core::{AppState, HostConfig};
use farm_host::build_router;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let config = HostConfig::from_env()?;
    let bind = config.bind;
    tracing::info!(
        %bind,
        contract = %farm_sdk::CONTRACT_VERSION,
        "farm-host starting (loopback by default)"
    );

    let state = AppState::new(config)?;
    // 启动对账（ADR-0005 §9）：发现映射在而 worktree 目录缺/损时重建或标 broken。
    // 非致命——单票重建失败只标 broken，交由 merge 服务拒收，不阻塞宿主启动。
    match state.workspace.reconcile() {
        Ok(report) => {
            if !report.rebuilt.is_empty() || !report.marked_broken.is_empty() {
                tracing::info!(
                    rebuilt = report.rebuilt.len(),
                    marked_broken = report.marked_broken.len(),
                    "workspace startup reconcile"
                );
            }
        }
        Err(e) => tracing::warn!(error = %e, "workspace startup reconcile failed (non-fatal)"),
    }
    let router = build_router(state);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "farm-host listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("farm-host stopped");
    Ok(())
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

/// 有序停机：Ctrl-C 或 SIGTERM。（插件侧「关 stdin 即停」归 #19。）
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut sig) = signal(SignalKind::terminate()) {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
