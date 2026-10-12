# 0005 — 持久化与状态层：SQLite 私有存储 + Kron 审计双写

- 状态：已接受（[票：持久化与状态层](https://github.com/JoeWest9966/Farm/issues/6)）
- 日期：2026-10
- 关联：ADR-0001（契约零域知识）、ADR-0002（面板持久化账单）、ADR-0003（单二进制）、ADR-0004（宿主进程模型）

## 背景

Symphony 规格故意只用内存态（重启即失），Farm 面板需要票历史、事件时间线、记账与崩溃恢复，必须自定持久化层。兄弟仓库 Kron 是 append-only 哈希链审计账（Go，子进程调用），需划清分工。

## 决策

1. **存储引擎：嵌入式 SQLite**（rusqlite + WAL）。面板的过滤查询、tracker 归一化视图、记账聚合是关系型负载；sled 半弃维护且查询弱；自管文件要自造 journal。单文件配合单二进制，备份 = 拷文件。
2. **持久/易失分界**：
   - 持久（SQLite 表）：归一化票快照与状态历史、每票 attempts 时间线（含 token 归集）、可过滤事件日志、spec↔集成分支↔worktree 映射、覆盖层当前态与标记、通知通道配置、workflow 配置 last-known-good 缓存。
   - 易失（内存）：调度循环瞬态（重试定时器、in-flight 派发）、全量 transcript（agent 存活期可看，死后只留时间线摘要）、插件进程句柄。
3. **与 Kron 分工 = 双写**：SQLite 是面板唯一读源；Kron 是审计真源。宿主在关键事件点（派发、merge、审批、stall 判杀、host_restarted）经 `ledger record` 子进程补账。Kron 写失败不阻塞主流程，只告警——审计是旁路，不是运行依赖。
4. **崩溃恢复**：照抄 Symphony 骨架 + 一层记账。启动时所有 `running` attempt 标 `interrupted`（终态），worktree 不清（保留现场），事件日志补 `host_restarted`；重派与否由编排器按 workflow 配置的重试策略决定，宿主不自作主张（契约零域知识）。
5. **读写权：表按五服务分域，SQLite 是宿主私有实现细节**。插件与面板都不直连 DB，一切读走契约 API / 宿主投影 API——否则 schema 沦为事实契约，迁移自由尽失。
6. **覆盖层落 Farm SQLite**（per-仓库 keyed），不落被编排仓库：写进仓库会污染 PR diff、违反「agent 永不碰仓库源目录」。
7. **数据库生命周期：全局单库**，`$XDG_DATA_HOME/farm/farm.db`，仓库作为一列；版本化顺序 migration。
8. **双写一致性：接受窗口 + 事后补账**。事件带 `audited` 标记位，`ledger record` 成功置位；启动对账扫描未置位事件批量补写（ledger 幂等）。
9. **worktree 半写：映射先写、worktree 后建**（write-ahead 映射记录）。启动对账发现映射在而目录缺/损，重建或标 `broken`；merge 服务拒绝 broken 映射。
10. **保留策略**：events 表有界（保留窗口），超限归档进 Kron（冷层）；attempts 时间线永久保留。具体数值归宿主/workflow 配置。
11. **interrupted 的记账**：以最后一次流式回流的用量快照为准 + 事件标 `accounting_truncated`；宿主边收事件边记账，不等终态，宁少报不虚报。

## 后果

- SQLite schema 永不外泄为契约；对插件的稳定面只在 farm-sdk。
- 审计链完整性靠启动补账保住，Kron 宕机不影响 Farm 运行。
- `interrupted` 成为 attempt 的第四种终态（成功/失败/interrupted 之外的记账语义见 GLOSSARY）。
