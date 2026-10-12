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

## 捕俘 / Capture（SuctionCup 语境）

把一台猎物服务器变成集群节点的全过程：连接 → 检测 → 装 Crossline 后门 → 灌载荷。SuctionCup 是 Farm 的独立插件（自己的 WIT/Wasm ABI），**不属于编排器契约**；它做的是 AFK 的前置，不碰 agent 生命周期。
_避免_: 把吸盘当 agent-runner（它不驱动 agent）
