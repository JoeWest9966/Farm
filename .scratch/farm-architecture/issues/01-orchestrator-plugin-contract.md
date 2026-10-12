# 01 - Farm 的编排器插件契约（本图中心问题）

Type: grilling
Status: open

## Question

Farm 定义怎样的"编排器插件契约"，使 Orchestrator（独立通用 AFK 编排器，多 tracker：GitHub/Gogs，未来服务电商等非编程 AFK）能作为进程级 IPC 插件接入，同时契约对其他编排器实现保持开放？

必须覆盖（结合 /tmp/farm-research/report-skills.md 的 9 行接口表与 report-symphony.md 的 §10）：
- 任务图与 frontier 的表达：ticket 状态机、dispatchable 判定。
- 完成信号语义：必须是"合入集成分支"而非"票关闭"（implement-spec 的已记录失败模式）。
- agent 派发接口：编排器是否直接驱动 pi，还是通过 Farm 的 agent-runner？派发与事件回流的协议形状。
- 多 tracker 抽象（GitHub/Gogs/native）。
- 契约中"编程域假设"与"通用 AFK 假设"的边界——哪些字段/阶段允许编码域知识，哪些必须中立。
