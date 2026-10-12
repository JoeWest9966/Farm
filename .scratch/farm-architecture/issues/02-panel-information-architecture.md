# 02 - 面板信息架构与功能清单

Type: grilling
Status: open

## Question

单用户驾驶舱面板显示什么、支持哪些操作？需产出功能清单与信息架构，至少裁决：
- AFK 监控视图：agent 运行状态、任务图/frontier 可视化、进度。
- token/成本记账视图（Symphony 的绝对累计记账法可借鉴）。
- HITL 介入：面板上如何向运行中的 agent 注入指导/批准/打断（与 pi session 的对接点）。
- 审计回放：历史任务、Kron 审计日志的呈现深度。
- 面板读数据的通路：直接 IPC 订阅编排器事件，还是 Farm 宿主聚合后供前端拉取（参考 Symphony 的 Presenter 单投影层 + PubSub 单 topic）。

可选：先做低保真原型（prototype skill）再定稿。
