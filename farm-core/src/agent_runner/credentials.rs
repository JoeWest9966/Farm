//! 凭据注入接缝（GLOSSARY「过路财神」，ADR-0004 决策 3）。
//!
//! > agent-runner 在 spawn pi 前子进程调 `nw key get`，以环境变量注入子进程；Farm 零缓存、
//! > 零落盘，凭据在 Farm 进程内存中的存活时间 = 子进程 spawn 窗口。取凭据失败即 spawn
//! > 失败，走 `needs_attention`。
//!
//! 本票（#16）只落地**接口与注入面**：`CredentialCourier` 由 #17 实现（Warehouse 的
//! `nw key get` 子进程），这里保证三件事可被断言：
//!
//! 1. 凭据**只进 pi 子进程**（env 是单向注入的，宿主自己的 `env` 不被污染）；
//! 2. 取凭据失败 → spawn 失败 → `needs_attention`（不是 attempt failed 的重试语义）；
//! 3. 环境策略可被收窄成「只给这些变量」（测试与最小权限面用）。

use std::process::Command;

use crate::error::CoreError;

/// 环境注入策略：决定 pi 子进程看到哪些环境变量。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvPolicy {
    /// 继承宿主 env，再叠加 `extra`（默认：真 pi 需要 PATH/HOME/API key）。
    InheritPlus(Vec<(String, String)>),
    /// **只**给 `extra`（最小环境；测试与「凭据不得外溢」断言用）。
    Replace(Vec<(String, String)>),
}

impl Default for EnvPolicy {
    fn default() -> Self {
        EnvPolicy::InheritPlus(Vec::new())
    }
}

impl EnvPolicy {
    pub fn with_extra(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let pair = (key.into(), value.into());
        match &mut self {
            EnvPolicy::InheritPlus(extra) | EnvPolicy::Replace(extra) => extra.push(pair),
        }
        self
    }

    /// 应用到 `tokio::process::Command`（凭据只往这里走）。
    pub fn apply(&self, command: &mut Command) {
        match self {
            EnvPolicy::InheritPlus(extra) => {
                for (key, value) in extra {
                    command.env(key, value);
                }
            }
            EnvPolicy::Replace(extra) => {
                command.env_clear();
                for (key, value) in extra {
                    command.env(key, value);
                }
            }
        }
    }

    /// 本策略最终会让子进程看到的变量清单（测试断言用）。
    pub fn effective_pairs(&self) -> Vec<(String, String)> {
        match self {
            EnvPolicy::InheritPlus(extra) => {
                let mut pairs: Vec<(String, String)> = std::env::vars().collect();
                for (key, value) in extra {
                    pairs.retain(|(existing, _)| existing != key);
                    pairs.push((key.clone(), value.clone()));
                }
                pairs.sort();
                pairs
            }
            EnvPolicy::Replace(extra) => {
                let mut pairs = extra.clone();
                pairs.sort();
                pairs
            }
        }
    }
}

/// 凭据过路：spawn 前取用，返回只注入 pi 子进程的 env 对。
///
/// #17 用 `nw key get` 子进程实现它；本票的默认实现是「无凭据」。
pub trait CredentialCourier: Send + Sync + std::fmt::Debug {
    /// 取凭据。失败即 spawn 失败（走 `needs_attention`，不进插件重试策略）。
    fn collect(&self, request: &CredentialRequest) -> Result<Vec<(String, String)>, CoreError>;
}

/// 取凭据的输入（#17 按派内容物/ticket 决定取哪些 key）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialRequest {
    pub ticket: String,
    pub attempt_id: String,
    pub profile: Option<String>,
}

/// 默认凭据源：不取任何凭据（#17 之前的状态）。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCredentialCourier;

impl CredentialCourier for NoCredentialCourier {
    fn collect(&self, _request: &CredentialRequest) -> Result<Vec<(String, String)>, CoreError> {
        Ok(Vec::new())
    }
}

/// 测试/演示用的凭据源：固定 env 对（用来断言「只进子进程」）。
#[derive(Clone, Debug)]
pub struct StaticCredentialCourier {
    pub pairs: Vec<(String, String)>,
}

impl StaticCredentialCourier {
    pub fn new(pairs: Vec<(String, String)>) -> Self {
        Self { pairs }
    }
}

impl CredentialCourier for StaticCredentialCourier {
    fn collect(&self, _request: &CredentialRequest) -> Result<Vec<(String, String)>, CoreError> {
        Ok(self.pairs.clone())
    }
}

/// 总是失败的凭据源：验「取凭据失败 → spawn 失败 → needs_attention」。
#[derive(Clone, Copy, Debug, Default)]
pub struct FailingCredentialCourier;

impl CredentialCourier for FailingCredentialCourier {
    fn collect(&self, _request: &CredentialRequest) -> Result<Vec<(String, String)>, CoreError> {
        Err(CoreError::Config(
            "credential courier failed: warehouse unreachable".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_replace_policy_gives_the_child_only_the_injected_pairs() {
        let policy = EnvPolicy::Replace(vec![("FARM_TEST_SECRET".into(), "s3cret".into())]);
        assert_eq!(
            policy.effective_pairs(),
            vec![("FARM_TEST_SECRET".to_string(), "s3cret".to_string())],
            "Replace 策略下子进程看不到宿主 env——凭据不得外溢"
        );
    }

    #[test]
    fn the_inherit_plus_policy_keeps_the_host_environment_and_overrides_one_key() {
        let policy =
            EnvPolicy::default().with_extra("FARM_TEST_SECRET", "s3cret");
        let pairs = policy.effective_pairs();

        assert!(pairs.iter().any(|(key, value)| key == "PATH"));
        assert!(pairs
            .iter()
            .any(|(key, value)| key == "FARM_TEST_SECRET" && value == "s3cret"));
        assert_eq!(
            pairs.iter().filter(|(key, _)| key == "FARM_TEST_SECRET").count(),
            1,
            "同名 key 只能有一份（后者覆盖前者）"
        );
    }

    #[test]
    fn a_failing_courier_is_a_spawn_failure_not_an_attempt_failure() {
        let courier = FailingCredentialCourier;
        let error = courier
            .collect(&CredentialRequest {
                ticket: "16".into(),
                attempt_id: "att-1".into(),
                profile: None,
            })
            .expect_err("courier failure must surface");
        assert!(
            error.to_string().contains("warehouse unreachable"),
            "{error}"
        );
    }

    #[test]
    fn the_no_credential_courier_injects_nothing() {
        let courier = NoCredentialCourier;
        let pairs = courier
            .collect(&CredentialRequest {
                ticket: "16".into(),
                attempt_id: "att-1".into(),
                profile: None,
            })
            .expect("no credentials");
        assert!(pairs.is_empty());
    }
}
