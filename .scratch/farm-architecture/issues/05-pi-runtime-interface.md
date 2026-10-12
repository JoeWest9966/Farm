# 05 - pi 作为被驱动 agent 运行时的可编程面

Type: research
Status: resolved

## Question

Farm 要把 pi 当作被编排的 agent 运行时，需要摸清 pi 暴露的可编程接口：
- headless / SDK 模式：能否以编程方式启动一个会话、喂 prompt、收事件流（而非 TUI 交互）？
- session 事件、转写、token 用量是否可导出？
- 多会话并发、会话恢复/steer 的 API。
- pi 文档位置：/usr/lib/node_modules/@earendil-works/pi-coding-agent/（README.md、docs/、examples/，尤其 docs/sdk.md）。
- 结论要回答：Symphony 用 Port+JSON-RPC 驱动 Codex app-server 的模式，在 pi 上对应物是什么。

## Answer

报告全文：`../research/pi-runtime-interface.md`。

**对应物是 `pi --mode rpc`：子进程 + stdin/stdout 双向 JSONL（自定义 `{type, command, id}` 行协议，非 JSON-RPC 2.0、非 HTTP、非 SDK 内嵌）。**

- 34 个命令 + 22 种事件类型，覆盖 prompt/steer/abort/会话切换/统计——即 Symphony 手写 Codex 客户端层的那一层 pi 已内置。
- token 记账、`get_entries` 的 durable entry-id 转写游标（跨重启有效）、`agent_settled` 终态锚点全部现成。
- `/skill:name` 会被 pi 展开 ⇒ 编排器可直接注入 `/skill:implement-spec`，无需程序化复刻 9 步协议（推翻 skills 报告中“只能复刻”的悲观结论）。
- 架构印证：pi 官方集成面就是子进程 + 行分隔 JSON，与 Farm 的进程级 IPC 插件方向同构。

留给下游票的硬点：
1. RPC 子进程终止语义文档未明确（需三段式收尾）→ 票 04 宿主进程模型。
2. 扩展 UI 对话框若不带 timeout 会永久阻塞宿主 → 票 02 面板 HITL 通道必须应答。
