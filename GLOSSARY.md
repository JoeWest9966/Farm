# Farm 术语表

本表是 Farm 及其兄弟生态的概念权威。中文优先，英文专名随行。禁用同义词在每条末尾。

## 编排器契约 / Orchestrator Contract

Farm 暴露给编排器类插件的一组**宿主服务 API**。Farm 是服务端，编排器是客户端/驱动进程；Farm 提供被编排所需的原语（tracker、agent 派发、merge、workspace、事件），不提供编排决策本身。
_避免_: 插件协议（过宽）、调度接口（方向反了——Farm 不问插件下一步派谁）

## 编排器 / Orchestrator

一个独立、通用（领域中立）的 AFK 编排器进程，以插件身份接入 Farm，跑自己的确定性循环，每 tick 调用编排器契约里的服务。与 `/implement-spec`（会话内 agent 编排）是并列的两个调度器，共享同一套词汇（spec、ticket、frontier、integration branch），不是上下级。
_避免_: 调度器（编排器包含调度但不止调度）、控制器（Crossline ADR-0003 已否决中央控制器拓扑）

## Agent-Runner

编排器契约五服务之一：以统一接口派发与管理 agent（spawn `pi --mode rpc` 子进程、事件回流、steer/abort、token 记账、`agent_settled` 判终态），实现带可插后端——本地后端直接管 pi 子进程；远程后端把同一接口翻译到 Crossline wire 之上。编排器不感知 agent 跑在哪台机器上。
_避免_: agent 池（不含生命周期语义）、执行器

## Tracker 服务

五服务之一：归一化 Issue 模型 + 原生 blocking 边 + **原子 claim**（CAS 语义，assignee 即 claim），把 provider 语义（GitHub/Gogs/native）关进 adapter。面板与编排器读同一个归一化视图——**tracker 是总线**。frontier/dispatchable 的计算是编排器的职责，不在服务内。
_避免_: issue 网关、tracker 抽象层（口语句可用，正式名就叫 Tracker 服务）

## Merge 服务

五服务之一：宿主侧确定性的合并执行者。agent 只能**提议**合并；服务校验（worktree 干净、FF 可行、测试过）后执行真正的 merge。**完成信号 = 集成分支上出现该 ticket 的 merge commit**，是宿主可验证的机械事实，永不采信 agent 自述。
_避免_: merger（那是 implement-spec 里的 subagent 角色）

## 完成信号 / Completion Signal

一张票"做完"的唯一权威判据：其改动合入集成分支的 merge commit。关票、开 PR 等都只是完成**之后**的投影。区别于"票关闭"——implement-spec 时代已记录的失败模式。
_避免_: 票关闭即完成（明确否决）

## Workspace 服务

五服务之一：拥有"spec ↔ 集成分支 ↔ worktrees"映射关系并负责 worktree 的创建与清理；映射关系持久化（存储形态归持久化票裁决）。merge 服务与面板读写映射、不拥有它。
_避免_: 工作区管理器

## 事件总线 / Event Bus

五服务之一：编排器与 agent-runner 的统一事件回流面，面板只做投影层。`blocked` / `needs_attention` 是一等事件（claim 保留、面板必渲染、人类经 steer/follow_up 回应）。
_避免_: 日志流、消息队列

## Workflow 配置 / Workflow Config

住在**被编排仓库**里的机器可读调度策略文件：dispatchable 判定标签、并发配额、重试退避、stall 阈值、派发 prompt 模板、投影开关（如是否开 PR）。消费者是编排器的确定性循环。非法时保 last-known-good。
_避免_: WORKFLOW.md（Symphony 的具体形态，Farm 只继承思路）、skill（那是给 LLM 的）

## Skill

pi 运行时展开的 markdown 指令（`/skill:name`），定义 **agent 行为**（怎么实现一张票、tdd、review），随被编排仓库/profile 走。消费者是 LLM。域知识按消费者二分：LLM 吃 skill，死循环吃 workflow 配置；契约本体零域知识。
_避免_: 把调度策略写进 skill（确定性程序读不了 markdown）

## 原子 Claim / Atomic Claim

tracker 服务提供的防双跑原语：对一张票的排他所有权声明，CAS 语义，失败即已被他人抢走。任何调度器（编排器、人肉 `/implement-spec` 会话）必须先 claim 再干活；派发前 revalidation 是调度器侧义务。
_避免_: 锁（claim 是票据状态，不是技术锁）

## 投影 / Projection

完成信号触发后的确定性写回动作（关票、评论 merge commit 链接、开 PR、打标签）。由宿主机械执行，由 workflow 配置开关；agent 的职责在 merge 提议时结束。
_避免_: 收尾（口语化）、写回（Symphony 语境里指 agent 写回——已否决那种做法）

## 操作台 / Operator Console

面板的定位：不止于监控投影，而是**全功能操作台**——凡可操作的功能最终都要在面板上可达。凡编排器契约里有的操作，面板以契约客户端的身份执行（与编排器同路、同原子 claim）；宿主自身的装卸与配置走独立的**宿主管理 API**。演进按批次：先读与打断，再调度操作，后管理与审阅。
_避免_: 仪表盘（只读含义太弱）、驾驶舱（口语句可用，正式名是操作台）

## 宿主管理 API / Host Admin API

面板专用的宿主自身管理面（插件装卸、tracker 配置、服务启停），与编排器契约并存但边界分明——不是契约的私有后门。
_避免_: 管理端点（无边界感）、私有大后门

## 覆盖层 / Overlay

面板对 workflow 配置基线的**临时运行时覆盖**：显式标记、重启后提示、不静默污染仓库文件。文件是被编排仓库里的基线，覆盖是面板的特权写。
_避免_: 运行时配置（分不清是基线还是覆盖）、配置写回（覆盖不落盘）

## 审阅流 / Review Flow

人在面板上审阅 agent 的 merge 提议（看 diff、批准或拒绝）的流程。与 merge 服务的机械校验叠加：机械闸门之上的人工闸门，属于面板第三批能力。
_避免_: code review（那是 agent 间行为）、PR 流程

## 通知通道 / Notification Channel

关键事件（`blocked`、`needs_attention`、merge 完成、stall）的宿主侧外发出口，可插拔。真 AFK 的人不在屏幕前，通知是 HITL 的前半程。事件集闭包 = 上述四类关键事件保证进事件总线且可订阅；默认过滤集 = blocked / needs_attention / stall 开、merge 完成关（常态流量，票看板已是其投影）。
_避免_: 推送（方向反了——是人被通知，不是面板推数据）

## Notifier 插件 / Notifier Plugin

通知通道的一等实现形态：进程级 IPC 插件，订阅事件总线、自决载体（URL/headers/body 模板、重试、限流）。宿主零推送域知识，推送格式全住插件侧。第一方只做一个通用 HTTP notifier（覆盖 ntfy/Bark/Discord/Slack 这类裸 POST 即达的服务）；Telegram 等专用格式插件留给第三方，不进 Farm 仓库。它消费事件、不驱动循环，与编排器类插件（契约客户端）是两类插件。
_避免_: 内置转发器（已否决——为通知另开第二条扩展路径）、webhook 出口（那是某个具体插件的载体，不是通道本身）

## 捕俘 / Capture（SuctionCup 语境）

把一台猎物服务器变成集群节点的全过程：连接 → 检测 → 装 Crossline 后门 → 灌载荷。SuctionCup 是 Farm 的独立插件（自己的 WIT/Wasm ABI），**不属于编排器契约**；它做的是 AFK 的前置，不碰 agent 生命周期。
_避免_: 把吸盘当 agent-runner（它不驱动 agent）

## 派内容物 / Pie Payload

随派（profile）装配进 agent 运行环境的东西：skill、extension、pi 版本（recipe 的 runtime_pins）、Sentry 闸门等。Farm 宿主对派内容物只装配与消费其外发事件，不监督其内部。pi 运行时版本是派内容物，不是 Farm 的钉死项。

_避免_: 插件（那是 Farm 宿主的进程级扩展，方向相反——派内容物住在 agent 侧）

## 过路财神 / Credential Courier

Farm 在凭据上的角色：spawn 前经 `nw key get` 取用、以 env 注入子进程、零缓存零落盘，凭据在 Farm 内存中的存活时间 = spawn 窗口。Farm 不是凭据保管人，Warehouse 才是。

_避免_: 凭据代理（暗示可续存）、密钥管理器

## 隔离 / Quarantined

插件进程重试耗尽后的状态：宿主停止重启、发 `needs_attention`、面板可一键恢复。防崩溃风暴的硬边界。

_避免_: 熔断（无自动恢复语义——恢复是人裁决）、死亡标记（quarantined 可恢复）

## 判据阶梯 / Judgement Ladder

agent-runner 本地后端对 pi 子进程的监督规格（源自 Slaveholder 六篇 ADR）：读循环健康度前置 → 两口钟（静默钟+总钟）→ abort 探针（30s 独立死线）→ fork 找回。完成判定 = `agent_settled` 且最后一条权威 assistant 消息 `stopReason === "stop"` 双条件。pi 子进程不是插件，不进插件重试策略。

_避免_: 重试策略（那是插件监督的事）、超时（一个词装不下两口钟）

## 宿主私有存储 / Host-private Store

Farm 宿主的嵌入式 SQLite（WAL）：面板与编排器的唯一读源（经契约/投影 API），但 schema 是宿主私有实现细节，永不外泄为契约。持久项：票快照与状态历史、attempts 时间线、事件日志、workspace 映射、覆盖层、通知通道配置、last-known-good。易失项：调度瞬态、全量 transcript、进程句柄。

_避免_: 数据库（口语句可用，正式名强调私有性）、状态层（太泛）

## 审计双写 / Audit Double-write

关键事件（派发、merge、审批、stall 判杀、host_restarted）既进宿主私有存储（面板读源）又经 `ledger record` 子进程进 Kron 账本（审计真源）。Kron 写失败不阻塞主流程；未入账事件带 `audited` 标记位，启动时批量补账（ledger 幂等）。审计是旁路，不是运行依赖。

_避免_: 事件镜像（暗示同步复制）、审计日志（那是 Kron 本身）

## 中断态 / Interrupted

attempt 的第四种终态：宿主重启时所有 `running` attempt 被标记的记账状态。非成功非失败，worktree 不清（保留现场），token 记账以最后快照为准并标 `accounting_truncated`。是否重派由编排器按 workflow 配置裁决，宿主不自作主张。

_避免_: 失败（重试语义不同——interrupted 不触发退避计数）、孤儿（那是进程视角的词）
