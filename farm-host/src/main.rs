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
    // 崩溃恢复（ADR-0005 §4，ticket #20）：running attempt → interrupted（保留现场）+
    // host_restarted 事件入账 + 审计旁路批量补账（幂等）。重派与否留给编排器。
    match farm_core::recovery::recover_on_startup(&state) {
        Ok(report) => {
            if !report.interrupted.is_empty() || report.backfilled > 0 {
                tracing::info!(
                    interrupted = report.interrupted.len(),
                    backfilled = report.backfilled,
                    "host crash recovery"
                );
            }
        }
        Err(e) => tracing::warn!(error = %e, "host crash recovery failed (non-fatal)"),
    }
    // 插件骨架（#19，ADR-0004 决策 4）：启动对账（上次 running 的子进程已随宿主死）+
    // 按清单拉起插件。launch 只等第一次握手（有超时上界），崩溃由监督循环按分级退避
    // 重试，不阻塞宿主启动。清单经 FARM_PLUGINS（逗号分隔的程序路径）注入。
    if let Err(e) = farm_core::plugin::reconcile_on_startup(&state.plugin) {
        tracing::warn!(error = %e, "plugin startup reconcile failed (non-fatal)");
    }
    for spec in plugin_manifest() {
        match state.plugin.launch(&state, &spec).await {
            Ok(hello) => tracing::info!(
                plugin = %spec.plugin_id,
                contract = %hello.contract_version,
                "plugin launched and handshook (contract client, no private backdoor)"
            ),
            Err(e) => tracing::warn!(
                plugin = %spec.plugin_id,
                error = %e,
                "plugin launch failed (supervision retries per tiered backoff)"
            ),
        }
    }
    let plugin_host = state.plugin.clone();
    let router = build_router(state);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "farm-host listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    // 宿主停机：有序收尾所有插件（关 stdin → 宽限 → 强杀），一个孤儿都不留（#19）。
    plugin_host.shutdown_all().await;
    tracing::info!("farm-host stopped");
    Ok(())
}

/// 插件清单：`FARM_PLUGINS`（逗号分隔的程序路径）→ 每条一个 [`farm_core::plugin::PluginSpec`]。
/// 每个插件以契约客户端身份接入（与编排器、面板同路），宿主不为它开私有接口。
fn plugin_manifest() -> Vec<farm_core::plugin::PluginSpec> {
    std::env::var("FARM_PLUGINS")
        .ok()
        .map(|list| {
            list.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|program| {
                    let id = std::path::Path::new(program)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| program.to_string());
                    farm_core::plugin::PluginSpec::new(id, program, program)
                })
                .collect()
        })
        .unwrap_or_default()
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
