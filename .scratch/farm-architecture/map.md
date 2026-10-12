# Farm 架构规划（wayfinder:map）

## Destination

一份可直接开工的 Farm 完整架构 spec：模块划分、插件协议（尤其编排器插件契约）、面板功能清单、与各兄弟仓库（Orchestrator/Crossline/Warehouse/Kron/Sentry/nw 等）的集成方案。spec 交付即地图完成，实现代码不在本图内。

## Notes

- Farm：Rust 全栈（前端 WASM + 后端）AFK 编程平台，带面板，可插拔插件架构。语言定夺不再复议。
- 深读源码的四份报告是所有会话的必读上下文：`/tmp/farm-research/report-symphony.md`、`report-skills.md`、`report-orch-crossline.md`、`report-nous-family.md`（若 /tmp 被清，仓库可从 GitHub 重新浅克隆重读）。
- 已锁定的决定（不复议）：终点为架构 spec；前端 Rust→WASM；插件为进程级 IPC；面板为单用户驾驶舱；agent 运行时锁定 pi；兄弟仓库保持独立、Farm 以 IPC 插件接入现状；拓扑 = Farm 本体是宿主+面板+插件骨架（无编排逻辑），Orchestrator 为独立通用 AFK 编排器、以插件身份接入，Farm 只定义编排器插件契约。
- 每次 grilling 会话调用 grilling + domain-modeling 两个 skill。
- 术语沉淀进根目录 `GLOSSARY.md`。

## Decisions so far

- [pi 可编程面](issues/05-pi-runtime-interface.md)：驱动面是 `pi --mode rpc`（子进程 + stdin/stdout JSONL，34 命令/22 事件），`/skill:implement-spec` 可直接注入；详见 `research/pi-runtime-interface.md`。

## Not yet specified

- 通用性反哺：电商等其他领域 AFK 的需求如何约束编排器插件契约，使其不过度绑定编程域（等契约 ticket 出雏形后回来收）。
- Gogs 作为 tracker 的支持细节（契约要留位，但支持本身可能在 Orchestrator 的地图里，不在 Farm 图里）。
- Crossline 接入时机与形态（传输层插件 vs 直接用其未来 Rust core crate；依赖其 1/10 的重写进度）。
- 吸盘（部署工具）与 Warehouse 作为 Farm 插件的优先级排序。
- 安全与权限模型（Symphony 三大空白之一；等进程模型定稿）。
- 面板与 Agent 的 HITL 介入通道细节（等面板信息架构定稿）。

## Out of scope

- 用 Rust 重写任何兄弟仓库（Crossline 自己在重写，Farm 不代劳）。
- 多用户 / 团队面板（登录、权限、协作）。
- 实现代码本身——本图终点是 spec，写代码是下一张图/下一个努力。
- 电商 AFK 本体的设计（Orchestrator 的通用性只需在契约里留位）。
