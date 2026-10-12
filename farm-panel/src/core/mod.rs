//! # 面板数据核（panel data core，ticket #22）
//!
//! ADR-0002 §4：面板只消费宿主**投影层**（快照 API + 增量推送），不直连编排器或 pi；
//! ADR-0002 §1：面板是**契约的另一个客户端**——claim/release、steer/follow_up、abort、
//! `clear_queue` 全部走 `farm_sdk::routes::CONTRACT_ROUTES` 里的路由，与编排器同路、
//! 同一枚 op token，**没有私有后门**。
//!
//! Dioxus CSR 的 `view` 没法在无浏览器环境里测，所以面板的数据层整体抽到这个模块：
//! 它是一个**纯函数式的契约客户端 + 视图模型 reducer**，不碰 DOM、不碰 wasm API、
//! 不持任何 I/O——请求由它拼、响应由它解、事件由它归集。这让两件事可验证：
//!
//! 1. **无私有后门**：[`PanelOp`] 是闭集，每个操作产出的路径都在契约路由表里
//!    （`tests/contract_client.rs` 逐条断言）。wasm 侧的传输层只会把 [`HttpRequest`]
//!    发给 `fetch`，自己没有拼路径的能力。
//! 2. **看板/下钻的归集逻辑**：快照 + WS 增量 → 看板列归属、HITL 一等队列、
//!    token 三维归集、attempts 时间线、只读 transcript，全部有单元测试。
//!
//! #24/#25 在它上面继续盖 UI（dispatch、覆盖层、宿主管理面、审阅流）：新操作先加进
//! [`PanelOp`]，路由先在契约表里存在，测试再跟上。
//!
//! ## 模块地图
//!
//! | 模块 | 职责 |
//! | --- | --- |
//! | [`client`] | 契约客户端：拼请求、解响应/WS 帧（唯一知道路径的地方） |
//! | [`board`] | 看板视图模型：列归属 + `blocked`/`needs_attention` 一等队列 |
//! | [`tokens`] | token 三维归集（全局/每票/每天，绝对累计防重） |
//! | [`drill`] | 单票下钻：attempts 时间线、轮次、只读 transcript、workspace |
//! | [`rhythm`] | 连接节律：什么时候该取快照、什么时候该拉增量（WS 不可用时） |

pub mod client;
pub mod tokens;

pub use client::{
    parse_ws_frame, ContractClient, Decoded, HttpRequest, HttpResponse, PanelError, PanelOp,
    WsFrame, OP_TOKEN_HEADER, PANEL_CLIENT_NAME,
};

pub use tokens::{
    day_key, tokens_from_event, tokens_summary, TokenAggregate, TokenPayload, TokenTotals,
};
