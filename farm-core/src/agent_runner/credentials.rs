//! 凭据注入接缝（GLOSSARY「过路财神」，ADR-0004 决策 3）。
//!
//! > agent-runner 在 spawn pi 前子进程调 `nw key get`，以环境变量注入子进程；Farm 零缓存、
//! > 零落盘，凭据在 Farm 进程内存中的存活时间 = 子进程 spawn 窗口。取凭据失败即 spawn
//! > 失败，走 `needs_attention`。
//!
//! 本模块先由 #16 落地**接口与注入面**（`CredentialCourier` trait + `EnvPolicy` + 几个测试替身），
//! 保证三件事可被断言：
//!
//! 1. 凭据**只进 pi 子进程**（env 是单向注入的，宿主自己的 `env` 不被污染）；
//! 2. 取凭据失败 → spawn 失败 → `needs_attention`（不是 attempt failed 的重试语义）；
//! 3. 环境策略可被收窄成「只给这些变量」（测试与最小权限面用）。
//!
//! #17 落地**真凭据源** [`WarehouseCourier`]：spawn 前子进程调 `nw key get`，解析 Warehouse
//! 的 JSON envelope，把 env 对交回 spawn 窗口注入 pi 子进程。Farm 仍零缓存零落盘——凭据在
//! Farm 进程内存中的存活时间 = spawn 窗口。另留 [`credentials_leaked_into`] 作 #19 联测接缝。

use std::process::Command as StdCommand;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::CoreError;

/// 环境注入目标：`std` 与 `tokio` 两种 `Command` 都能接（凭据只往这里走）。
///
/// 宿主 spawn pi 子进程用的是 `tokio::process::Command`（读循环是异步的），而
/// `EnvPolicy` 的判据与测试不关心是哪一种——故按能力抽象，不按类型抽象。
pub trait EnvTarget {
    fn set_env(&mut self, key: &str, value: &str);
    fn clear_env(&mut self);
}

impl EnvTarget for StdCommand {
    fn set_env(&mut self, key: &str, value: &str) {
        self.env(key, value);
    }
    fn clear_env(&mut self) {
        self.env_clear();
    }
}

impl EnvTarget for tokio::process::Command {
    fn set_env(&mut self, key: &str, value: &str) {
        self.env(key, value);
    }
    fn clear_env(&mut self) {
        self.env_clear();
    }
}

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

    /// 应用到 pi 子进程的命令（凭据只往这里走）。
    ///
    /// 泛型在 [`EnvTarget`] 上：`tokio::process::Command`（宿主异步 spawn 用的）
    /// 与 `std::process::Command` 都能接。
    pub fn apply(&self, command: &mut impl EnvTarget) {
        match self {
            EnvPolicy::InheritPlus(extra) => {
                for (key, value) in extra {
                    command.set_env(key, value);
                }
            }
            EnvPolicy::Replace(extra) => {
                command.clear_env();
                for (key, value) in extra {
                    command.set_env(key, value);
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

/// 解析 `nw key get <key>` 子进程打印的**一份 Warehouse JSON envelope**，取出要注入的 env 对。
///
/// Warehouse 契约（`docs/research/report-nous-family.md` §2.4）：stdout 只有一份 envelope，
/// `{"schema":"nw/key.v1","ok":<bool>,"errors":[...],"data":{"env":..,"value":..}}`，
/// `ok == (exitCode == 0)`。env 变量名由 **Warehouse 说了算**（GLOSSARY：Farm 是过路财神，不是
/// 凭据保管人）——Farm 只忠实注入它返回的 `data.env`/`data.value`。
///
/// fail-closed：退出码非 0、`ok=false`、缺 `data.env`/`data.value`、或 stdout 不是合法 JSON，
/// 一律 `Err`（→ spawn 失败 → `needs_attention`），绝不静默降级成空凭据。
///
/// 独立真源是 Warehouse 的字面量契约；本函数是纯函数（无子进程），单测按已知字面量断言。
pub fn parse_nw_key_response(
    key: &str,
    exited_ok: bool,
    stdout: &str,
) -> Result<(String, String), CoreError> {
    let envelope: Value = serde_json::from_str(stdout).map_err(|e| {
        CoreError::Config(format!(
            "`nw key get {key}` did not print a JSON envelope: {e}"
        ))
    })?;
    let ok = envelope.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if !exited_ok || !ok {
        let errors = envelope.get("errors").cloned().unwrap_or(Value::Null);
        return Err(CoreError::Config(format!(
            "`nw key get {key}` failed (exit_ok={exited_ok}, ok={ok}): {errors}"
        )));
    }
    let data = envelope.get("data").ok_or_else(|| {
        CoreError::Config(format!("`nw key get {key}` envelope carried no data"))
    })?;
    let env = data
        .get("env")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CoreError::Config(format!("`nw key get {key}` data has no env var name to inject"))
        })?
        .to_string();
    let value = data
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| CoreError::Config(format!("`nw key get {key}` data has no value")))?
        .to_string();
    Ok((env, value))
}

/// **#19 联测接缝**：凭据 env 是否泄漏进了某个进程环境。
///
/// 给定「观测到的进程 env」与「凭据 env 对」（本票 fake nw 返回的，或 #19 自备样本），返回
/// 同时命中「键相同且值相同」的键名——空 = 没泄漏。插件宿主（#19）spawn 插件进程后用它机械
/// 校验 ADR-0004 决策 3 的不变量：凭据只进 pi 子进程，绝不外溢到插件进程环境。
///
/// Farm 从不把凭据注入插件进程：凭据只经 [`crate::agent_runner::process::PiSpawnPlan`] 进 pi
/// 子进程，插件 env 由插件宿主另行装配，两者结构隔离。这条断言是联测时对那条不变量的兜底。
pub fn credentials_leaked_into(
    observed_env: &[(String, String)],
    credential_pairs: &[(String, String)],
) -> Vec<String> {
    credential_pairs
        .iter()
        .filter(|(key, value)| {
            observed_env
                .iter()
                .any(|(obs_key, obs_value)| obs_key == key && obs_value == value)
        })
        .map(|(key, _)| key.clone())
        .collect()
}

/// Warehouse `nw key get` 凭据源：GLOSSARY「过路财神」/ ADR-0004 决策 3 的 #17 落地。
///
/// Farm 是过路财神，不是保管人：spawn 前为每个 key 子进程调 `nw key get <key> [--repo <p>]`，
/// 解析 Warehouse 的 JSON envelope，把它说的 env 对交回 spawn 窗口注入 pi 子进程。**零缓存**
/// （每次 [`CredentialCourier::collect`] 都重新跑 `nw`）、**零落盘**（只把 stdout 收进内存
/// 解析，绝不写任何文件）——凭据在 Farm 进程内存中的存活时间 = spawn 窗口（即 `collect` 返回的
/// `Vec` 被移进 spawn 计划、子进程起来后原值即被丢弃的那一段）。
///
/// `nw` 二进制路径**可注入**（测试指向 fake nw；生产用 PATH 上的 `nw`）。`nw` 子进程的额外
/// env 也可注入（Warehouse 认 `NW_HOME` 等 env 覆盖；测试用它隔离 fake nw 的场景配置，
/// 绝不污染宿主自己的 env——凭据注入是单向的）。
#[derive(Clone, Debug)]
pub struct WarehouseCourier {
    /// `nw` 可执行文件（可注入；缺省在 PATH 上找 `nw`）。
    program: String,
    /// 要取的 key 名清单（每个跑一次 `nw key get <key>`）。
    keys: Vec<String>,
    /// 可选 `--repo <path>`：数据仓位置（Warehouse 两仓取货）。
    repo: Option<String>,
    /// 单个 `nw key get` 子进程的超时（到点强杀并判失败，绝不久悬 spawn 窗口）。
    timeout: Duration,
    /// 注入 `nw` 子进程的额外 env（如 `NW_HOME`；测试用它传 fake nw 场景）。
    env: Vec<(String, String)>,
}

impl WarehouseCourier {
    /// 默认子进程超时：10s。真 `nw` 是个快 CLI；久悬即当失败，绝不卡住 spawn 窗口。
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

    /// 构造：`program` 指向 `nw`（测试传 fake nw 绝对路径），`keys` 是要取的凭据名。
    pub fn new(program: impl Into<String>, keys: Vec<String>) -> Self {
        Self {
            program: program.into(),
            keys,
            repo: None,
            timeout: Self::DEFAULT_TIMEOUT,
            env: Vec::new(),
        }
    }

    /// 数据仓位置（`--repo <path>` 传给 `nw`）。
    pub fn with_repo(mut self, repo: impl Into<String>) -> Self {
        self.repo = Some(repo.into());
        self
    }

    /// 覆写单个 `nw key get` 子进程的超时。
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 给 `nw` 子进程加一对 env（只进该子进程，不污染宿主 env）。
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// 跑一个 `nw key get <key>`，返回解析出的 env 对（失败即 spawn 失败）。
    fn fetch_key(&self, key: &str) -> Result<(String, String), CoreError> {
        let mut command = StdCommand::new(&self.program);
        command.arg("key").arg("get").arg(key);
        if let Some(repo) = &self.repo {
            command.arg("--repo").arg(repo);
        }
        for (env_key, env_value) in &self.env {
            command.env(env_key, env_value);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (status, stdout, stderr) = run_to_completion_with_timeout(command, self.timeout)?;
        parse_nw_key_response(key, status.success(), &stdout).map_err(|error| match stderr {
            stderr if !stderr.trim().is_empty() => CoreError::Config(format!(
                "{error} (nw stderr: {})",
                stderr.trim()
            )),
            _ => error,
        })
    }
}

impl CredentialCourier for WarehouseCourier {
    fn collect(&self, _request: &CredentialRequest) -> Result<Vec<(String, String)>, CoreError> {
        // 零缓存：每次都真的为每个 key 跑一次 `nw`，不读任何本地副本。
        let mut pairs = Vec::with_capacity(self.keys.len());
        for key in &self.keys {
            pairs.push(self.fetch_key(key)?);
        }
        Ok(pairs)
    }
}

/// 同步跑一个子进程到结束，带超时；返回 `(exit_status, stdout, stderr)`。
///
/// stdout/stderr 由独立线程抽干（避开管道缓冲区写满导致的死锁），主线程轮询 `try_wait`
/// 到点或超时。超时即强杀并判失败——绝不让他人（`nw` 卡死）拖住 spawn 窗口。纯 `std`，无新依赖。
fn run_to_completion_with_timeout(
    mut command: StdCommand,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, String, String), CoreError> {
    use std::io::Read;
    let mut child = command.spawn().map_err(CoreError::Io)?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut pipe) = stdout {
            let _ = pipe.read_to_string(&mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut pipe) = stderr {
            let _ = pipe.read_to_string(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().map_err(CoreError::Io)? {
            Some(status) => {
                let out = out_reader.join().unwrap_or_default();
                let err = err_reader.join().unwrap_or_default();
                return Ok((status, out, err));
            }
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = out_reader.join();
                    let _ = err_reader.join();
                    return Err(CoreError::Config(format!(
                        "credential subprocess timed out after {}ms",
                        timeout.as_millis()
                    )));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
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

        assert!(pairs.iter().any(|(key, _)| key == "PATH"));
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

    // --- #17: Warehouse `nw key get` envelope 解析（纯函数，无子进程）---

    #[test]
    fn a_well_formed_nw_key_envelope_yields_the_env_pair_warehouse_dictates() {
        // 独立真源 = Warehouse 契约字面量（`ok==true` + `data.{env,value}`）；不重算实现。
        let stdout = r#"{"schema":"nw/key.v1","nw":"0.1.0","ok":true,"errors":[],"warnings":[],"data":{"env":"FARM_GH_TOKEN","value":"ghp_passthrough_literal"}}"#;
        let pair = parse_nw_key_response("github-token", true, stdout)
            .expect("a well-formed ok envelope must yield the env pair");
        assert_eq!(pair, ("FARM_GH_TOKEN".to_string(), "ghp_passthrough_literal".to_string()));
    }

    #[test]
    fn an_nw_key_envelope_with_ok_false_is_a_spawn_failure_even_on_exit_zero() {
        let stdout = r#"{"schema":"nw/key.v1","ok":false,"errors":[{"code":"FORBIDDEN","message":"denied"}],"data":null}"#;
        let error = parse_nw_key_response("github-token", true, stdout)
            .expect_err("ok=false must surface as a spawn-blocking error");
        let text = error.to_string();
        assert!(text.contains("github-token"), "{text}");
        assert!(text.contains("FORBIDDEN"), "错误要带上 warehouse 的原因: {text}");
    }

    #[test]
    fn a_nonzero_nw_exit_is_a_spawn_failure_even_if_stdout_looks_fine() {
        // exit!=0 一律失败，绝不因为 stdout 像成功就放行（fail-closed）。
        let stdout = r#"{"schema":"nw/key.v1","ok":true,"data":{"env":"X","value":"y"}}"#;
        let error = parse_nw_key_response("github-token", false, stdout)
            .expect_err("a non-zero exit must fail the spawn");
        assert!(error.to_string().contains("github-token"), "{error}");
    }

    #[test]
    fn an_nw_key_envelope_missing_the_env_name_is_a_spawn_failure() {
        let stdout = r#"{"schema":"nw/key.v1","ok":true,"errors":[],"data":{"value":"orphan"}}"#;
        let error = parse_nw_key_response("github-token", true, stdout)
            .expect_err("data without an env var name cannot be injected");
        assert!(error.to_string().contains("env"), "{error}");
    }

    #[test]
    fn non_json_nw_output_is_a_spawn_failure_not_a_silent_empty() {
        let error = parse_nw_key_response("github-token", true, "not json at all")
            .expect_err("unparseable output must fail, never degrade to empty");
        assert!(error.to_string().contains("JSON envelope"), "{error}");
    }

    // --- #17: #19 联测接缝 —— 凭据 env 绝不泄漏进插件进程环境 ---

    #[test]
    fn a_missing_nw_binary_fails_the_spawn_fail_closed() {
        // 配置里 nw 路径写错：spawn 子进程直接 Io 失败 → collect 报错（绝不静默注入空凭据）。
        let courier = WarehouseCourier::new(
            "/nonexistent/nw-binary-ticket-17",
            vec!["github-token".to_string()],
        )
        .with_timeout(Duration::from_secs(2));
        let error = courier
            .collect(&CredentialRequest {
                ticket: "17".into(),
                attempt_id: "att-missing".into(),
                profile: None,
            })
            .expect_err("a missing nw binary must fail the spawn, never inject nothing");
        assert!(
            matches!(error, CoreError::Io(_)),
            "fail-closed on a missing nw binary: {error}"
        );
    }

    #[test]
    fn credentials_leaked_into_reports_only_exact_key_and_value_matches() {
        let credentials = vec![("FARM_GH_TOKEN".to_string(), "ghp_secret".to_string())];
        // 插件进程 env 里有同名但不同值 → 不算泄漏（凭据值没过去）。
        let observed = vec![
            ("FARM_GH_TOKEN".to_string(), "someone-elses".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
        ];
        assert!(
            credentials_leaked_into(&observed, &credentials).is_empty(),
            "值不同不算泄漏"
        );
        // 键值都中 → 报出来。
        let leaked = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("FARM_GH_TOKEN".to_string(), "ghp_secret".to_string()),
        ];
        assert_eq!(
            credentials_leaked_into(&leaked, &credentials),
            vec!["FARM_GH_TOKEN".to_string()]
        );
    }
}
