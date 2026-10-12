# 03 - Rust 全栈技术选型

Type: grilling
Status: open

## Question

具体框架与部署形态：
- 前端：Dioxus vs Leptos（vs Yew）——评估生态、SSR 需求、实时更新（服务端推送到 WASM 前端的通路：WebSocket/SSE）。
- 后端：axum（或替代）作为宿主 HTTP/WS 服务。
- 部署形态：单二进制（前端 WASM 嵌入二进制，参考 Symphony 的 Burrito 单文件发布思路）？
- monorepo 布局：workspace crate 划分（farm-core / farm-plugin-sdk / farm-panel / 插件骨架）。
