//! 宿主运行时覆写（ticket #26 加法接通）：让**真实 farm-host 二进制**在受控场景里
//! 跑在 fake 替身之上（fake-pi / fake-nw / fake-ledger），从而端到端 tracer 与一致性
//! 套件能对真宿主进程驱动整链。
//!
//! ## 为什么需要它（整链暴露的真缺口）
//! 宿主默认本地后端跑真 `pi`、无凭据源、merge 闸门跑 `cargo test`、tracker 空原生票源。
//! 端到端演练要把这些换成 fake 替身——但换 program / 凭据源 / 测试命令 / 票源**不是契约面**
//! （契约零域知识，ADR-0001 决策 4），而是**宿主进程配置**。故经环境变量在启动时覆写，
//! 与 `FARM_BIND` / `FARM_TOKEN` / `FARM_LEDGER` / `FARM_PLUGINS` 同路（`main` 装配时读），
//! 不是私有后门：宿主仍是服务端，客户端照旧只过契约 HTTP/WS。
//!
//! ## 默认行为一字不变
//! 无一覆写时 `apply` 原样返回 `state`：真 `pi`、无凭据、`cargo test` 闸门、空票源——
//! 所以默认宿主与既有一切测试行为不变。只覆写被显式配置的那一项。
//!
//! ## 凭据只进子进程（过路财神不变）
//! fake-nw 的数据（`FAKE_NW_KEYS`）与 fake-pi 的日志（`FAKE_PI_LOG`）/场景（`FAKE_PI_SCENARIO`）
//! 都**不经这里**：它们由宿主 env 被 pi / nw 子进程**继承**（`EnvPolicy::InheritPlus` +
//! `WarehouseCourier` 的 `StdCommand` 默认继承父 env），凭据值仍只在 spawn 窗口内存里活。
//! 宿主自己的两个鉴权 token（`FARM_TOKEN` / `FARM_ADMIN_TOKEN`）是例外：`EnvPolicy::apply`
//! 收尾无条件扣下它们（code review P1-1）——继承面收窄到「真 pi 需要的环境」，不收窄到
//! 「宿主管理面的钥匙」。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::agent_runner::credentials::{CredentialCourier, NoCredentialCourier, WarehouseCourier};
use crate::agent_runner::{AgentRunner, RunnerConfig};
use crate::error::CoreError;
use crate::merge::{MergeConfig, MergeService};
use crate::state::AppState;
use crate::tracker::{NativeAdapter, TrackerService};

/// merge 闸门覆写时的死线（受控场景：fake 测试命令是即返的；仍是硬上界，防挂住）。
const OVERRIDE_TEST_TIMEOUT: Duration = Duration::from_secs(120);

/// 从环境变量读来的运行时覆写（ticket #26 加法接通）。
///
/// 每个 `Option`/`Vec` 都是「要不要覆写对应服务」的开关；空 = 不覆写（保默认）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeOverrides {
    /// agent-runner 的 pi program（`FARM_AGENT_PROGRAM`；测试指向 fake-pi）。
    pub agent_program: Option<String>,
    /// 凭据源 program（`FARM_CREDENTIAL_PROGRAM`；测试指向 fake-nw）。
    pub credential_program: Option<String>,
    /// 要取的凭据 key 名清单（`FARM_CREDENTIAL_KEYS`，逗号分隔）。
    pub credential_keys: Vec<String>,
    /// 数据仓位置（`FARM_CREDENTIAL_REPO`，可选）。
    pub credential_repo: Option<String>,
    /// merge 闸门的测试命令（`FARM_MERGE_TEST_COMMAND`，空白分隔；测试指向快脚本）。
    pub merge_test_command: Option<Vec<String>>,
    /// 原生票源 JSON 路径（`FARM_NATIVE_TICKETS`）。
    pub native_tickets_path: Option<PathBuf>,
}

impl RuntimeOverrides {
    /// 从进程环境读取（`main` 用）。
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// 从任意查找器读取（可测：测试注入受控 map，不碰进程 env）。
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let get = |key: &str| lookup(key).filter(|value| !value.trim().is_empty());
        Self {
            agent_program: get("FARM_AGENT_PROGRAM"),
            credential_program: get("FARM_CREDENTIAL_PROGRAM"),
            credential_keys: split_csv(lookup("FARM_CREDENTIAL_KEYS")),
            credential_repo: get("FARM_CREDENTIAL_REPO"),
            merge_test_command: get("FARM_MERGE_TEST_COMMAND")
                .map(|command| command.split_whitespace().map(str::to_string).collect()),
            native_tickets_path: get("FARM_NATIVE_TICKETS").map(PathBuf::from),
        }
    }

    /// 是否配置了 agent-runner 覆写（program 或凭据源任一即需重建 runner）。
    pub fn touches_agent_runner(&self) -> bool {
        self.agent_program.is_some() || self.credential_program.is_some()
    }

    /// 把覆写应用到宿主状态；只碰被配置的服务，其余原样。
    ///
    /// 存储故障 / 票源文件读不动才返回 `Err`（投影为契约 `internal`）——那是运营者配错了，
    /// 启动即失败比静默跑在错误配置上好。
    pub fn apply(&self, mut state: AppState) -> Result<AppState, CoreError> {
        if self.touches_agent_runner() {
            let mut config = RunnerConfig::default();
            if let Some(program) = &self.agent_program {
                config.program = program.clone();
            }
            let courier = self.credential_courier();
            let runner = AgentRunner::new(state.db.clone())
                .with_config(config)
                .with_credentials(courier);
            state = state.with_agent_runner(runner);
        }

        if let Some(command) = &self.merge_test_command {
            if !command.is_empty() {
                let merge = MergeService::with_config(
                    state.db.clone(),
                    MergeConfig {
                        test_command: command.clone(),
                        test_timeout: OVERRIDE_TEST_TIMEOUT,
                    },
                );
                state = state.with_merge(merge);
            }
        }

        if let Some(path) = &self.native_tickets_path {
            let adapter = NativeAdapter::from_json_path(path)?;
            let db = state.db.clone();
            state = state.with_tracker(TrackerService::new(db, Arc::new(adapter)));
        }

        Ok(state)
    }

    /// 凭据源：配了 program 就是 [`WarehouseCourier`]（过路财神），否则无凭据。
    fn credential_courier(&self) -> Arc<dyn CredentialCourier> {
        match &self.credential_program {
            Some(program) => {
                let mut courier =
                    WarehouseCourier::new(program.clone(), self.credential_keys.clone());
                if let Some(repo) = &self.credential_repo {
                    courier = courier.with_repo(repo.clone());
                }
                Arc::new(courier)
            }
            None => Arc::new(NoCredentialCourier),
        }
    }
}

/// 逗号分隔的 key 清单（去空白、丢空段）。
fn split_csv(raw: Option<String>) -> Vec<String> {
    raw.map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup_from(map: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + 'static {
        let owned: HashMap<String, String> = map
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| owned.get(key).cloned()
    }

    #[test]
    fn an_empty_environment_configures_nothing() {
        let overrides = RuntimeOverrides::from_lookup(lookup_from(&[]));
        assert_eq!(overrides, RuntimeOverrides::default());
        assert!(!overrides.touches_agent_runner());
    }

    #[test]
    fn the_agent_program_and_credential_source_are_read_from_the_environment() {
        let overrides = RuntimeOverrides::from_lookup(lookup_from(&[
            ("FARM_AGENT_PROGRAM", "/bin/fake-pi"),
            ("FARM_CREDENTIAL_PROGRAM", "/bin/fake-nw"),
            ("FARM_CREDENTIAL_KEYS", "github-token, deploy-key ,"),
            ("FARM_CREDENTIAL_REPO", "/repo"),
        ]));
        assert_eq!(overrides.agent_program.as_deref(), Some("/bin/fake-pi"));
        assert!(overrides.touches_agent_runner());
        assert_eq!(
            overrides.credential_keys,
            vec!["github-token".to_string(), "deploy-key".to_string()],
            "逗号分隔 + 去空白 + 丢空段"
        );
        assert_eq!(overrides.credential_repo.as_deref(), Some("/repo"));
    }

    #[test]
    fn only_a_credential_source_still_touches_the_agent_runner() {
        // 只配凭据源也要重建 runner（默认 runner 是 NoCredentialCourier）。
        let overrides =
            RuntimeOverrides::from_lookup(lookup_from(&[("FARM_CREDENTIAL_PROGRAM", "/bin/fake-nw")]));
        assert!(overrides.agent_program.is_none());
        assert!(overrides.touches_agent_runner());
    }

    #[test]
    fn the_merge_test_command_is_split_on_whitespace() {
        let overrides = RuntimeOverrides::from_lookup(lookup_from(&[
            ("FARM_MERGE_TEST_COMMAND", "./run-tests.sh --fast"),
        ]));
        assert_eq!(
            overrides.merge_test_command,
            Some(vec!["./run-tests.sh".to_string(), "--fast".to_string()])
        );
    }

    #[test]
    fn a_blank_value_is_treated_as_unset() {
        let overrides = RuntimeOverrides::from_lookup(lookup_from(&[
            ("FARM_AGENT_PROGRAM", "   "),
            ("FARM_MERGE_TEST_COMMAND", ""),
        ]));
        assert_eq!(overrides.agent_program, None);
        assert_eq!(overrides.merge_test_command, None);
        assert!(!overrides.touches_agent_runner());
    }

    #[test]
    fn the_native_tickets_path_is_read_as_a_path() {
        let overrides = RuntimeOverrides::from_lookup(lookup_from(&[
            ("FARM_NATIVE_TICKETS", "/tmp/tickets.json"),
        ]));
        assert_eq!(overrides.native_tickets_path, Some(PathBuf::from("/tmp/tickets.json")));
    }

    #[test]
    fn a_bare_host_apply_is_a_no_op_on_the_agent_runner_and_merge() {
        // 默认（无覆写）apply：agent-runner 仍跑默认 program（pi），merge 仍默认 cargo test。
        let state = crate::AppState::new_in_memory(crate::HostConfig {
            bind: "127.0.0.1:3721".parse().expect("bind"),
            op_token: "t".into(),
            admin_token: None,
            db_path: std::env::temp_dir().join("farm-runtime-noop.db"),
        })
        .expect("state");
        let before_program = state.agent_runner.config().program.clone();
        let before_merge = state.merge.config().test_command.clone();
        let applied = RuntimeOverrides::default().apply(state).expect("apply");
        assert_eq!(applied.agent_runner.config().program, before_program);
        assert_eq!(applied.merge.config().test_command, before_merge);
    }

    #[test]
    fn the_agent_program_override_reaches_the_runner_config() {
        let state = crate::AppState::new_in_memory(crate::HostConfig {
            bind: "127.0.0.1:3721".parse().expect("bind"),
            op_token: "t".into(),
            admin_token: None,
            db_path: std::env::temp_dir().join("farm-runtime-agent.db"),
        })
        .expect("state");
        let overrides = RuntimeOverrides {
            agent_program: Some("/bin/fake-pi".into()),
            ..Default::default()
        };
        let applied = overrides.apply(state).expect("apply");
        assert_eq!(applied.agent_runner.config().program, "/bin/fake-pi");
    }

    #[test]
    fn a_bad_native_tickets_path_is_an_error_not_a_silent_empty() {
        let state = crate::AppState::new_in_memory(crate::HostConfig {
            bind: "127.0.0.1:3721".parse().expect("bind"),
            op_token: "t".into(),
            admin_token: None,
            db_path: std::env::temp_dir().join("farm-runtime-tickets.db"),
        })
        .expect("state");
        let overrides = RuntimeOverrides {
            native_tickets_path: Some(PathBuf::from("/nonexistent/tickets-for-t26.json")),
            ..Default::default()
        };
        assert!(
            overrides.apply(state).is_err(),
            "票源文件读不动要报错（运营者配错了），不静默跑空票源"
        );
    }
}
