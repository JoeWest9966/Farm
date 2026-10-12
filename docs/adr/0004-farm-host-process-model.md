# 0004 - Farm 宿主进程模型与兄弟仓库集成方式

日期: 2025-06（wayfinder 票 [#4](https://github.com/JoeWest9966/Farm/issues/4) 裁决）
状态: 已接受

## 背景

Farm 宿主进程的运行时形态未定：谁 spawn 并监督 pi agent 子进程；Kron（账本）、Sentry（审批闸门）、Warehouse（装配与凭据）这些兄弟仓库以什么身份接入；插件进程崩了怎么办；以及 pi 版本在 Sentry/Slaveholder（钉 0.84.3）与 Warehouse（pin 1.0.2）之间的偏斜如何裁决。这份裁决是安全与权限模型票的前置。

## 决策

**五项裁决**（对应票内五问）：

1. **agent-runner 自 spawn、自监督；Sentry 是派内容物，不是进程监督者。**
   agent-runner 本地后端直接 spawn `pi --mode rpc` 子进程并持有全部生命周期，判据学照抄 Slaveholder 六篇 ADR（完成双条件 `agent_settled` + `stopReason === "stop"`、两口钟、读循环健康度前置、abort 探针、fork 找回）。Slaveholder 仓本身不进 Farm——它的 ADR 是规格，不是依赖。Sentry 闸门经 Warehouse 装进派里，Farm 只消费 Kron 账本中的 pending/终态事件。

2. **Kron 直接作为审计账，且只是审计账。**
   Farm 子进程调 `ledger record/read`，事件 schema 照抄（Rust 侧 serde + deny_unknown_fields 映射）。边界画死：Kron **不是** Farm 的持久化层——tracker/workspace/配置的状态存储归持久化票另裁。面板读账走低频 + `--agg` 侧车；read 事件进「只豁免不刷新」表。前置条件：与 Kron 作者确认 `actor` 闭集扩位加入 `farm`（现闭集 `{sentinel, whitehelmet, slaveholder, crossline, human}`）。

3. **凭据通路 = spawn 期过路取用。**
   agent-runner 在 spawn pi 前子进程调 `nw key get`，以环境变量注入子进程；Farm 零缓存、零落盘，凭据在 Farm 进程内存中的存活时间 = 子进程 spawn 窗口。取凭据失败即 spawn 失败，走 `needs_attention`。Farm 是凭据的过路财神，不是保管人。

4. **插件监督 = 宿主监督 + 分级退避重试 + 隔离。**
   - 插件是宿主子进程，宿主持有其 stdin（关 stdin 即请求有序停机，父死子陪葬）；
   - 分级指数退避重试（1s/10s/1min，上限 3 次），重启后插件从 Farm 服务读状态自愈（契约本就是无状态 tick 循环）；
   - 重试耗尽 → 隔离（quarantined），发 `needs_attention`，不无限重启（防崩溃风暴）；
   - 崩溃/重启进 Kron 账（escalation 事件，`actor=farm`）；
   - 面板提供 quarantined 一键恢复；
   - pi agent 子进程不是插件：归 agent-runner 判据阶梯管，不进插件重试策略。

5. **pi 版本归配方钉；Farm 自身基准 = 1.0.2。**
   运行时版本由 Warehouse recipe 的 `runtime_pins` 决定，Farm 不越权；Farm 的开发/CI/契约一致性套件基准钉 1.0.2（与 Warehouse pin 对齐，避免自家生态三版本并存）。spec 写死「换 pi 版本必重读清单」：Sentry 的 `gate_failed` 触发差异、`stopReason` 白名单七值、`agent_settled` 语义、事件流字段名。Sentry 在 1.0.2 上的重验是 spec 的前置任务。

   > **勘误（2026-10-12，据 Sentry 在 1.0.2 上的重验实测，`docs/research/sentry-pi-1.0.2-revalidation.md` §4）**：`gate_failed` 触发差异**不是**「0.84.3 = fail-safe（放行）vs 1.x = fail-closed（拦）」——**两个版本实测都是拦下**，fail-safe 这个前提不成立。真差别在**静默**：hook 抛异常时留不留 `extension_error` 遥测、异常文本是否泄进对话。零遥测 + 文本进对话 ⇒ 外部不可区分「闸门自己崩了」与「闸门照常放行」（Pi 1.0.0 事故同一族），所以闸门的铁律是「自己失败 = fail-closed + 硬上限 + 心跳自证」，换 pi 版本要重读的也正是这一条（拦不拦 + 崩了看不看得见），不是「哪个版本放行」。

## 考虑过的替代方案

- **委托 Sentry/Slaveholder 管 agent 进程**——被否：Sentry 自己声明不做执行编排，Slaveholder 零代码；委托会把活性真相源推出 Farm，观测面分裂（与 ADR 0001 否决 Crossline 当 agent-runner 同一理由）。
- **Kron 兼任持久化层**——被否：Kron ADR 0003 承诺「只回看不重演」，编排状态从事件流投影等于让账本做它明确不做的事。
- **Farm 自建事件账**——被否：Kron 五命令全实现、全测试绿、契约字段级冻结，重写是纯重复建设。
- **Farm 维护自己的凭据库**——被否：违反「兄弟仓库保持独立」，重复 Warehouse，且制造第二个机密落盘点。
- **Farm 钉死运行时 pi 版本**——被否：版本是派的内容物，钉死越权且挡住各派按需升级。

## 后果

- Farm 宿主 = 服务端 + 插件监督者 + agent 监督者（经 agent-runner），不引入任何兄弟仓库作为进程监督中间层。
- 与 Kron 作者有一项跨仓协调义务（actor 闭集扩位）。
- Sentry-on-1.0.2 重验成为 spec 前置任务；换 pi 版本有明确的重读清单。
- 安全与权限模型的「凭据」一角已闭合（过路财神模式），其余（网络面、插件权限）待后续票。
