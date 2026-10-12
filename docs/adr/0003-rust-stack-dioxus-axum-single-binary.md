# 0003 — Rust 全栈技术选型：Dioxus CSR + axum + 单二进制

- 状态：已接受（[票：Rust 全栈技术选型](https://github.com/JoeWest9966/Farm/issues/3)）
- 日期：2025-12
- 关联：ADR-0001（编排器契约）、ADR-0002（操作台）

## 背景

Farm 宿主要同时承载：编排器契约（HTTP/WS 服务 API）、宿主管理 API、面板供数。前端已定 Rust→WASM，需要敲定具体框架、后端框架、部署形态与 workspace 布局。Symphony 研究报告明确前端形态是纯自由决策点，无迁移障碍。

## 决策

1. **前端：Dioxus，纯 CSR 模式。** 不需要 SEO/SSR（单用户操作台）；选 Dioxus 是为热重载与类 React 心智模型，服务面板三批次快速迭代（ADR-0002）。锁 Dioxus 稳定版本线，1.0 发布后升级。否决 Leptos（杀手锏 SSR 用不上）与 Yew（迭代放缓）。
2. **后端：axum。** tokio/tower 官方生态，WS 与 SSE 开箱即用，承接契约三个面。
3. **面板实时通路：WebSocket，单连接订阅事件总线。** 操作（claim/steer/abort）走契约 HTTP API，WS 只承事件投影流；选 WS 为多流复用与扩展面。
4. **部署形态：单二进制。** 前端 WASM 产物经 rust-embed 嵌入 axum 静态路由；cargo-dist 出多平台发布产物。Rust 天生静态编译，无需 Burrito 类打包。单文件符合「集群节点是捡来的服务器」的部署世界观（吸盘式捕俘）。
5. **workspace 布局：五分法。**
   - `farm-sdk`：契约类型 + 宿主管理 API schema，纯类型 crate、零运行时依赖；插件方与面板都只 depend 它（正名 sdk 而非 plugin-sdk：面板也是契约客户端）。
   - `farm-core`：五服务实现 + 事件总线 + tokio runtime。
   - `farm-panel`：Dioxus CSR crate。
   - `farm-host`（bin）：axum 路由 + rust-embed 面板 + 插件骨架装配，唯一 binary。
   - 插件 crate **不进 Farm workspace**：进程级、独立版本，只靠 `farm-sdk` 对齐。`examples/` 下放不进 workspace 的参考骨架（orchestrator-skeleton、suction-cup-skeleton），CI 单独编译，作为契约冒烟测试。

## 后果

- 契约零域知识从「文档承诺」升级为「编译期可验证」。
- 面板与宿主之间无同构/水合复杂度；CSR 冷启动换来部署极简。
- Dioxus 稳定线与 1.0 之间的 API 变动风险由锁版本线对冲。
