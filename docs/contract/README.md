# Farm 编排器契约（orchestrator contract）

> **GENERATED — 勿手改。** 本文件由 `farm-sdk` 类型单一源渲染：
> `cargo run -p farm-sdk --example gen_contract_docs`
> 测试 `farm-sdk/tests/contract_docs_are_generated.rs` 断言本文件与生成结果逐字节一致。

- 契约版本（contract version）：`1.0.0`（主版本号 `1`，握手只比主版本）
- 宿主版本（host version）：`0.1.0`
- farm-sdk 版本：`0.1.0`

## 鉴权（auth）

单一 op token，静态配置。**所有契约路由（含 `/handshake`）与 WS upgrade 都要验**；
缺 token 与错 token 返回**同一个**闭集错误 `unauthorized`（HTTP 401，WS upgrade 同样 401 + 同一 JSON 体）。

| 面 | header | 说明 |
| --- | --- | --- |
| 契约 HTTP | `X-Farm-Token: <token>` | 或 `Authorization: Bearer <token>` |
| 契约 WS upgrade | 同上 | 握手同样验 token |
| 宿主管理 API | `X-Farm-Admin-Token: <token>` | 独立 token，与 op token 不是同一个 |

## 握手（handshake）

`POST /handshake` — 编排器接入第一跳。

- 请求：`HandshakeRequest`（`contract_version` + `client`）
- 响应：`HandshakeResponse`（契约版本、五服务清单、事件闭集、鉴权方式）
- 主版本不符 → 闭集错误 `contract_version_unsupported`（HTTP 400）

## 契约路由（v1 五服务）

| 服务 | 方法 | 路径 | 说明 | 本票状态 |
| --- | --- | --- | --- | --- |
| tracker | GET | `/v1/tracker/issues` | 归一化 Issue 视图（面板与编排器读同一视图） | 已通：归一化 Issue 视图（#13） |
| tracker | POST | `/v1/tracker/claims` | 原子 claim（CAS 语义，assignee 即 claim） | 已通：原子 claim（#13） |
| tracker | POST | `/v1/tracker/releases` | release：交还排他所有权（claim 是票据状态，不是技术锁） | 已通：release 交还排他所有权（#22） |
| agent-runner | POST | `/v1/agent-runner/dispatches` | 派发 agent（本地后端 spawn pi --mode rpc） | 已通：派发 attempt（#16） |
| agent-runner | POST | `/v1/agent-runner/attempts/{attempt_id}/steer` | 向运行中的 attempt 注入指令 | 已通：steer 注入（#16） |
| agent-runner | POST | `/v1/agent-runner/attempts/{attempt_id}/abort` | 中止 attempt（abort 探针） | 已通：abort 探针（#16） |
| agent-runner | POST | `/v1/agent-runner/attempts/{attempt_id}/follow-up` | follow_up：等 attempt 停下才送达的追加任务 | 已通：follow_up 注入（#22） |
| agent-runner | POST | `/v1/agent-runner/attempts/{attempt_id}/clear-queue` | clear_queue：清掉 attempt 上排队的注入，回报清了几条 | 已通：clear_queue（#22） |
| merge | POST | `/v1/merge/proposals` | agent 提议 merge（宿主校验后执行） | 已通：agent 提议 merge（#18） |
| merge | POST | `/v1/merge/reviews` | agent 提交 merge 提议进人工审阅队列（不自动执行；批准/拒绝走宿主管理 API，#25） | 已通：提议进人工审阅队列（#25） |
| workspace | GET | `/v1/workspace/mappings` | spec ↔ 集成分支 ↔ worktrees 映射 | 已通：workspace 映射视图（#14） |
| workspace | POST | `/v1/workspace/worktrees` | 建立 ticket worktree（write-ahead 映射 + git worktree add；#26 契约客户端整链接通） | 已通：契约客户端建 worktree（#26） |
| events | GET | `/v1/events/stream` | WS 单连接订阅事件总线（握手同样验 token） | 已通：WS 单连接事件投影（#15） |
| events | GET | `/v1/events/history` | 事件历史查询（有界保留窗口） | 已通：事件历史快照 API（#15） |

## 宿主管理 API（host admin API，#23）

独立前缀 `/admin`，独立 token header `X-Farm-Admin-Token`——**管理 token ≠ 操作 token**
（spec 新增裁决）。与编排器契约**边界分明**：管理面管宿主自身（插件装卸/恢复、tracker provider
配置、服务启停），契约面管编排原语；契约 API 无法触达任何管理面操作（不是私有后门）。

鉴权（`require_admin_token`）：管理 token 命中 → 放行；op token 递到管理面 → `forbidden`（403，
已鉴权但越面）；缺 / 错管理 token → `unauthorized`（401，同码不供预言机）；未配管理 token → 管理面禁用。

| 方法 | 路径 | 说明 | 本票状态 |
| --- | --- | --- | --- |
| GET | `/admin/plugins` | 插件清单与状态（#19 监督状态 → PluginSummary） | 已通：插件监督状态投影（#23 接 #19） |
| POST | `/admin/plugins/{id}/recover` | 恢复隔离/停机的插件（GLOSSARY「隔离」一键恢复，#19 recover） | 已通：恢复隔离/停机插件（#23） |
| GET | `/admin/status` | 宿主管理面自描述（契约版本 + 五服务） | 已通：管理面自描述 |
| GET | `/admin/tracker/provider` | 查询当前 tracker provider / adapter 配置（adapter 选择，#23） | 已通：tracker provider 配置 view/set（#23） |
| POST | `/admin/tracker/provider` | 设置 tracker provider / adapter（adapter 选择，#23） | 已通：tracker provider 配置 view/set（#23） |
| GET | `/admin/services` | 五服务启停状态（服务启停控制面，#23） | 已通：五服务启停状态（#23） |
| POST | `/admin/services/{name}/stop` | 停机一个服务（服务启停，#23；宿主持有开关，发 needs_attention） | 已通：停机一个服务（#23） |
| POST | `/admin/services/{name}/start` | 启动一个服务（服务启停，#23） | 已通：启动一个服务（#23） |
| GET | `/admin/workflow/config/{repo_id}` | 查询某被编排仓库当前生效的 workflow 配置与 LKG 状态（ticket #12） | 已通：workflow 配置 + LKG 查询（#12） |
| GET | `/admin/merge/proposals` | merge 审阅队列（人在面板上审阅提议，#25） | 占位：`not_implemented` |
| GET | `/admin/merge/proposals/{id}` | 单个 merge 提议详情 + diff（面板审阅呈现，#25） | 占位：`not_implemented` |
| POST | `/admin/merge/proposals/{id}/approve` | 批准 merge 提议：记审批决策入 Kron + 跑机械闸门出 merge commit（#25） | 占位：`not_implemented` |
| POST | `/admin/merge/proposals/{id}/reject` | 拒绝 merge 提议：记审批决策入 Kron + 闭集错误回 agent（#25） | 占位：`not_implemented` |
| GET | `/admin/notifier/channel/{repo_id}` | 查询某被编排仓库的通知通道配置：enabled + 过滤集（GLOSSARY「通知通道」，ticket #21） | 已通：通知通道配置 view/set（#21，宿主零推送域知识） |
| POST | `/admin/notifier/channel/{repo_id}` | 设置某被编排仓库的通知通道配置：enabled + 过滤集（重启后生效，ticket #21） | 已通：通知通道配置 view/set（#21，宿主零推送域知识） |

## 闭集错误码（closed-set error codes）

HTTP 响应体与 WS 文本帧共用同一信封：

```json
{"code":"not_implemented","message":"...","retryable":false}
```

| code | HTTP | 可重试 | 含义 |
| --- | --- | --- | --- |
| `unauthorized` | 401 | 否 | 缺 token / 错 token（同码，不做区分） |
| `forbidden` | 403 | 否 | 已鉴权但无权限 |
| `contract_version_unsupported` | 400 | 否 | 握手契约版本不被支持 |
| `invalid_request` | 400 | 否 | 请求不符合契约 schema |
| `not_found` | 404 | 否 | 路由不存在 |
| `method_not_allowed` | 405 | 否 | 路由存在但方法不对 |
| `conflict` | 409 | 否 | 状态冲突（如原子 claim 已被抢走） |
| `not_implemented` | 501 | 否 | 路由已占位、服务未实现 |
| `service_unavailable` | 503 | 是 | 宿主暂时不可用 |
| `internal` | 500 | 否 | 宿主内部错误 |

## 事件闭集（event closed set）

事件信封 `EventEnvelope`；`audited=false` 表示未入 Kron 账本（ADR-0005 §8 启动补账）。

| kind | 通知闭包 | 审计双写 | 含义 |
| --- | --- | --- | --- |
| `dispatch` | 否 | 是 | 派发：agent-runner 起了 attempt |
| `attempt_started` | 否 | 否 | attempt 生命周期占位：已启动 |
| `agent_settled` | 否 | 否 | attempt 生命周期占位：判据阶梯判终态 |
| `attempt_finished` | 否 | 否 | attempt 生命周期占位：已收尾 |
| `blocked` | 是 | 否 | 一等事件：attempt 卡住，claim 保留 |
| `needs_attention` | 是 | 否 | 一等事件：需要人类注意 |
| `steer` | 否 | 否 | 向运行中的 attempt 注入指令 |
| `abort` | 否 | 否 | 中止 attempt |
| `stall_kill` | 是 | 是 | 两口钟超时后判杀 |
| `merge` | 是 | 是 | merge 服务执行了真正的 merge |
| `approval` | 否 | 是 | 人工闸门放行 merge 提议 |
| `interrupted` | 否 | 否 | 宿主重启标记的第四终态 |
| `host_restarted` | 否 | 是 | 宿主重启（崩溃恢复） |
| `claim_acquired` | 否 | 否 | 原子 claim 成功 |
| `plugin_quarantined` | 否 | 否 | 插件重试耗尽进入隔离 |
| `plugin_restarted` | 否 | 是 | 插件崩溃后分级退避重启（进 Kron escalation） |
| `notification_sent` | 否 | 否 | 通知通道外发完成（保留：外发由宿主外的通知插件做，宿主零推送） |

## Schema

机器可读的单一源在 [`schema.json`](./schema.json)（JSON Schema 2020-12，`$defs` 覆盖全部契约类型）。
宿主私有存储的 schema **永不**进这里——SQLite 是宿主私有实现细节（ADR-0005 §5）。
