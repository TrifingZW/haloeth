# Haloeth 远程访问设计

Haloeth 的远程能力：桌面进程作为宿主，手机、平板等任意设备按公开协议接入，获得与桌面窗口趋近原生的完整交互——多会话、流式回复、思考过程、工具活动卡片、审批确认、用量与状态。

本文件是决策记录与集成方案；线格式（信封、操作目录、数据类型、错误码）单独定义在
[`remote-protocol.md`](remote-protocol.md)，两份文档互为引用、不重复定义。

## 1. 目标与非目标

目标：

- 协议优先：第三方可按规范实现客户端（原生 App、Web、CLI），不依赖本仓库代码。
- 趋近原生：远程端看到与桌面一致的内容（流式增量、活动卡片、审批卡片、计划、图片），能执行与桌面一致的动作（发送、取消、审批、会话管理、模型选择、功能开关、目录授权、审核模式）。
- 本地优先：不引入任何云中继与账号体系，桌面进程是唯一的宿主与事实来源。

非目标（v1 明确不做）：

- 不提供跨网中继服务；跨网络访问交给用户自建的隧道（Tailscale / WireGuard / frp 等）。
- 远程端不做偏好设置编辑（提供商、密钥、模型目录等跨会话配置仍只在桌面端管理，避免把密钥面暴露到线上协议）。
- 语音能力不在远程链路上传输（采集与播报是设备本地能力）。
- 不做独立无头服务进程（见 3.2 的演进方向）。

## 2. 总体形态

```text
手机 / 平板 / 浏览器 / CLI
        │  WebSocket（ws:// 或 wss://），JSON 帧
        ▼
┌─ remote crate（独立 Tokio 运行时线程）────────────────────┐
│  鉴权 · 解析 · 请求分发 · 通知扇出 · 背压 · 资源端点       │
└────────────┬─────────────────────────────▲───────────────┘
     命令队列（tokio mpsc）          通知广播（tokio broadcast）
             │                             │
┌─ haloeth 桥（GPUI 执行器上的泵任务）──────────────────────┐
│  命令 → 实体更新（复用既有动作） → 结果回传 → 状态发布    │
└────────────┬─────────────────────────────┬───────────────┘
             ▼                             ▼
   Haloeth / Session / Conversation   Session::submit 事件泵
   （主线程，唯一写入者）             （Event tap，逐事件投影）
```

三个关键角色：

| 角色       | 位置                    | 职责                                              |
| ---------- | ----------------------- | ------------------------------------------------- |
| 协议与服务 | 新 crate `remote`       | 线协议类型、WS 服务、连接状态机，零 GPUI 依赖     |
| 宿主桥     | `haloeth/src/remote.rs` | 实现 `RemoteHost`，把命令映射到既有动作，发布通知 |
| 事实来源   | 既有 GPUI 实体          | 单一写入者：所有状态变更仅发生在主线程            |

## 3. 关键决策

### 3.1 通信协议：WebSocket + 版本化 JSON

选定：RFC 6455 WebSocket 承载版本化 JSON 消息，信封形态对齐 JSON-RPC 的请求 / 响应 / 通知三分法，并逐帧携带协议大版本号（见协议规范第 4、10 节）。

理由：

- 双向长连接与现有事件架构匹配：代理运行产生的 `Event::Delta / Step / Activity` 与审批门输出本身是流，推送语义一一对应。
- 浏览器、iOS、Android 与各主流语言均有 WebSocket 客户端实现，客户端适配成本低。
- serde 类型即线上 schema 的单一来源，可作为 TypeScript 等类型生成的基础；文本帧可直接阅读与调试。
- 局域网、VPN、反向代理（Caddy / nginx 的 `wss://`）均可透明承载。

排除的备选：

| 备选         | 排除原因                                                                            |
| ------------ | ----------------------------------------------------------------------------------- |
| gRPC / tonic | 依赖体积大（与 `opt-level = "z"` 的体积优先策略冲突）；浏览器需 grpc-web 代理       |
| SSE + REST   | 服务端单向；命令需独立 HTTP 端点，形成双协议，请求关联需另行约定                    |
| WebRTC       | 面向媒体与 NAT 穿越场景；信令通道仍需自建，主要场景（局域网 / 隧道）不需要 NAT 穿越 |
| MCP          | 工具集成协议：不含流式 UI 语义、配对鉴权与会话镜像模型；本项目仅作为其客户端使用    |
| HTTP 轮询    | 流式增量场景下延迟与请求量不满足需求                                                |

编码选 JSON 而非 CBOR / MessagePack：对话规模下单帧数据量小，按可读性优先选择；压缩（`permessage-deflate`，依赖所选 WS 栈支持）不在 v1 范围内。

### 3.2 服务形态：进程内库 + 独立 crate

选定：新建 workspace 成员 `remote`（库 crate），由 `haloeth` 在进程内按需启动服务线程。

理由：

- 协议类型必须可被第三方客户端实现者独立引用，不能裹挟 GPUI 与前端模块。
- `agent` 是模型 / 工具 / 循环内核，远程镜像的是「界面状态」（transcript、审批、状态栏），置于其中会破坏「内核不感知 UI」的边界。
- 直接放 `haloeth` 里则协议 schema 与前端类型耦合，第三方需面对完整的 GPUI 依赖树。
- 服务与协议需要独立的无 UI 测试（回环连接测试），独立 crate 让这些测试不启动窗口。

排除的备选：

- 独立二进制（无头守护进程）：会话编排目前驻留在 GPUI 实体（`Session` / `Haloeth` / `Conversation`），抽出无头形态需要把编排层重构出实体系统，重构成本超出 v1 范围，且当前没有独立运行的需求。`RemoteHost` 契约不依赖 GPUI 类型，未来实现无头宿主时 `remote` crate 可原样复用，仅需在 `haloeth` 之外另行实现宿主——这是保留的演进方向。
- 独立进程 + IPC：镜像语义要求与桌面实体同进程，IPC 会增加一层转发，不提供对应收益。

体积注意：`axum` 引入 hyper 服务端栈；现有 `reqwest` 已带 hyper 客户端栈，增量主要是 tower / axum 本体。若 release 体积超出预算，备选是改用 `tokio-tungstenite` 实现最小服务（不含路由与静态资源能力），决策依据为实现期的体积测量。

### 3.3 交互语义：镜像模式

选定：远程客户端的操作直接作用于桌面端的当前状态——`session.open` 即桌面切换会话，与托盘菜单点击会话是同一种「远程控制」语义。

理由：

- 现有架构是单 `Session` 实体、单活动会话、串行运行（`Session::submit` 运行期间把代理移入运行任务），镜像模式与「单一事实来源 + 单一写入者」同构。
- 托盘（`background.rs` 的 `OpenSession`）已经验证了「另一个入口驱动主窗口」的模式，远程把同一动作面扩展到局域网。
- 客户端实现简单：无需合并逻辑，状态始终为服务端投影。

边界：多个客户端并发提交 / 审批时，命令在主线程泵上串行化，先到先得；审批的重复解决是幂等 no-op（`ApprovalHandle` 只唤醒一次等待者，见 11.3）。

未来扩展（不在 v1）：`session.read`（只读拉取非活动会话的存储 transcript，不切换桌面）可支撑「手机查看历史会话、桌面不跟随」的独立视角。

### 3.4 同步模型：语义推送 + 权威快照

选定：单一写入者（桌面）向所有连接广播细粒度语义通知（`message.delta`、`tool.started`、`approval.requested`…），在离散边界推送权威快照（`transcript.reset`、`state.changed`、`sessions.changed`）；客户端是纯 reducer。断线重连走全量重新同步（hello → welcome → reset）。通知信封携带宿主分配的单调递增 `seq`，客户端以已见最大值为游标；`session.resume` 从宿主历史环形缓冲（容量等于广播容量）补发缺失通知，不可补齐时回退快照（协议规范第 5 节）。

理由：

- 桌面 UI 对同一事件流的消费已经分两层：实时层（`Session::submit` 泵逐事件调用 `apply` / `apply_activity` / `handle_effect`）与持久层（状态切换到 `Idle / Failed / AwaitingApproval` 时 `save_active` 落盘 `TranscriptEntry`）。远程同步沿用同一分层：语义通知给实时渲染，快照给对账，两层的触发点与现有代码逐点对齐（见 6.3、6.4）。
- 无多写入者，不需要 CRDT / OT 的合并复杂度；慢客户端用「断开 + 重连全量同步」保证正确性（见 5.4）。
- 最终答案文本、错误回灌等「渲染规则」只存在于服务端一处（`render_step` 复用），客户端只做机械 reducer，避免各客户端重复实现桌面逻辑（单一事实来源）。

排除的备选：全量状态推送（每次变更重发整棵 transcript，流式期间量不可接受）、纯操作日志重放（重连需回溯日志序号，复杂度高于快照 + 全量重同步）。

### 3.5 安全模型：设备令牌 + 配对二维码

选定（分层，随阶段落地）：

1. v1 基线：每设备一条 256 位随机令牌，桌面端只存 SHA-256 哈希（`remote.devices`，可撤销、可轮换）；配对通过二维码（`haloeth://pair?host=…&port=…&token=…`）或手动复制；连接后首帧 `hello` 携带令牌或 `Authorization: Bearer` 头；校验失败与超时以关闭码终结，限速在升级阶段拒绝（HTTP 429）。
2. 传输加密：`wss://` + 运行时自签证书（`rcgen` + `rustls`），证书保存在应用数据目录 `Haloeth/remote-tls`；配对串追加证书指纹 `fp` 供客户端钉扎（TOFU，思路同 Syncthing 设备 ID）。
3. 跨网络：明确交由隧道（Tailscale / WireGuard 自带传输加密与访问面控制），项目不提供中继。

理由与核对：

- 与项目现状一致：配置明文、无沙箱是已声明边界；令牌哈希存储避免新增明文秘密面。
- 纯 `ws://` 局域网存在嗅探风险，这是 v1 的已声明边界（README「行为」同风格），阶段三的 TLS + 指纹钉扎予以收敛。
- 浏览器 `Origin` 校验 + 令牌不进 URL（除资源端点的 `<img>` 场景，见协议规范第 11 节）阻断跨站 WS 劫持。
- 命令面继承桌面审核链：远程端发出的消息同样经过四档审批，不构成审核绕过。

## 4. 工作区结构

```text
haloeth/
├── remote/                    新 crate（库，零 GPUI 依赖）
│   ├── Cargo.toml             tokio / serde / serde_json / uuid / axum(ws) / tokio-tungstenite / sha2 / tracing
│   ├── src/
│   │   ├── lib.rs             模块声明与重导出
│   │   ├── proto.rs           协议常量：版本、路径、上限
│   │   ├── proto/
│   │   │   ├── request.rs     请求信封、参数类型与解析后的命令枚举
│   │   │   ├── response.rs    响应信封与错误码
│   │   │   ├── notify.rs      通知信封与全部推送操作
│   │   │   └── dto.rs         线上数据类型（transcript / 状态 / 用量）
│   │   ├── host.rs            RemoteHost 契约与广播信箱
│   │   ├── server.rs          axum 路由、WS 升级、优雅停机
│   │   ├── conn.rs            每连接状态机：鉴权、分发、扇出、背压
│   │   ├── auth.rs            令牌生成与哈希校验
│   │   ├── tls.rs             自签证书生成与指纹计算
│   │   └── client.rs          参考客户端（含指纹钉扎）
│   ├── web/index.html         内置参考 Web 客户端（内联样式与脚本）
│   └── tests/
│       ├── wire.rs            线格式黄金快照（防意外破坏兼容性）
│       └── e2e.rs             回环连接：hello → 命令 → 通知断言
├── agent/                     不改动（内核不感知远程）
└── src/
    ├── remote.rs              新增：宿主桥（见第 6 节）
    ├── app/remote_tests.rs    新增：远程桥与偏好页的界面测试
    └── settings/remote.rs     新增：偏好页「远程」分页
```

依赖方向：`haloeth → remote`；`remote` 不依赖 `agent` 与 `haloeth`（协议 DTO 与内部类型解耦，见 6.4 的映射说明）。沿用「`<模块>.rs` 与同名目录并存、不用 `mod.rs`」的既有布局。

## 5. `remote` crate 设计

### 5.1 RemoteHost 契约

宿主能力接口保持最小面，由 `haloeth` 桥实现：

```rust
/// 宿主契约：把远程命令与状态读取投影到真实应用。
#[async_trait]
pub trait RemoteHost: Send + Sync + 'static {
    /// 校验设备令牌；通过则返回设备标识。
    fn authorize(&self, token: &str) -> Option<DeviceId>;
    /// 订阅通知流；连接期间的推送都来自它。
    fn subscribe(&self) -> broadcast::Receiver<Notification>;
    /// 分发一条已解析的强类型命令（含读操作），返回结果或结构化错误。
    async fn dispatch(&self, command: Command) -> Result<Value, RemoteError>;
}
```

设计要点：

- 参数解析与校验全部在服务端完成（未知操作名、参数不合法直接回 `unknown_op` / `invalid_params`，不进入宿主），宿主收到的 `Command` 已是强类型，仅负责执行与状态读取。
- `dispatch` 是唯一入口：读操作（`state.get`、transcript 组装）与写命令走同一条「回主线程」的通道，服务线程不区分读写，因此不存在绕开串行化的第二条路径。
- 契约不出现任何 GPUI / `agent` 类型；`Notification` / `Command` 是 `remote::proto` 的线类型。未来无头宿主可实现同一契约。
- 服务端不持有宿主状态的任何镜像，重连时全部经由 `dispatch` 取实时值。

### 5.2 服务生命周期

- `RemoteServer::start(options, host)`：在专用线程上创建 Tokio 运行时并绑定监听（与 `background::Background::start` 同构：独立线程 + `tracing` 日志 + 句柄交由 `Haloeth` 持有防止 drop）。`ServerOptions` 是 `remote` 自己的配置类型（监听地址、端口、限速参数），不依赖前端配置模型。
- 端点：`GET /v1`（WS 升级）、`GET /v1/assets`（图片资源）、`POST /v1/attachments`（附件上传）、`GET /`（内置 Web 客户端），均见协议规范第 11 节。路径带版本号，`/v2` 可并存。
- `RemoteServer::stop()`：关闭监听并等待连接排空（5 秒上限）。停用或重启服务前由桥先广播 `shutdown` 通知；偏好页开关即时生效，对应 start / stop。

### 5.3 连接状态机

```text
TCP/HTTP 建立 → WS 升级 → [鉴权窗口 ≤10s]
  ├─ Authorization 头携带令牌：直接校验
  └─ 否则首帧必须是 hello 请求：携带令牌校验
       ├─ 失败 → close 4403；超时未 hello → close 4408
       └─ 成功 → 返回 welcome（协议版本、服务端信息、初始 state 与 sessions）
            → 立即推送 transcript.reset（活动会话实时快照）
            → 进入稳态：请求串行处理，通知并行扇出
```

- 鉴权前收到其它帧 → close 4401；信封解析失败 → close 4400；单帧超过 1 MiB → close 4413。
- 请求按到达序串行 `dispatch`（每连接一个任务顺序 await），保证命令效果与桌面点击等价。
- 通知不排队在请求后面：独立任务从 broadcast 接收并写帧，读取缓慢只影响该连接自身（见 5.4）。
- 服务端每 20 秒发协议层 ping；`ping` 操作供浏览器类客户端做应用层保活与 RTT 测量。

### 5.4 背压与慢客户端

- 每连接一个有界转发任务（容量 1024 帧 / 4 MiB 预算），broadcast `Lagged` 或队列满 → close 4429。
- 客户端收到 4429 / 意外断开后的恢复路径统一为：退避重连 → 全量重新同步。快照模型保证该路径始终可用。
- 通知信封携带单调递增的 `seq`（协议规范第 4 节）；`session.resume` 按游标从宿主历史环形缓冲（容量等于广播容量）补发，超出窗口或会话不匹配时回退全量快照。
- 令牌校验失败计数按来源地址限速（每分钟 5 次），超限直接拒绝升级。

## 6. 桌面端桥接（haloeth）

### 6.1 线程模型

复用两条既有模式，不引入新机制：

| 数据流向          | 机制                                          | 既有先例                                                                         |
| ----------------- | --------------------------------------------- | -------------------------------------------------------------------------------- |
| 服务线程 → 主线程 | `tokio::sync::mpsc` + `cx.spawn` 泵消费       | `Session::submit` 的事件泵（等待 tokio 通道在 GPUI 执行器上 await 是已验证模式） |
| 主线程 → 工作线程 | `tokio::sync::broadcast`（`send` 为同步调用） | `TtsService` 的朗读 / 停止命令通道                                               |

约束核对：CLAUDE.md 明确「事件处理必须在实体更新之外完成」——泵任务在顶层逐条 `this.update(...)`，与后台事件轮询任务的结构一致；`cx.spawn` 的泵为事件驱动（`receiver.recv().await`）而非定时轮询，命令延迟为队列排队与一次实体更新的耗时。

### 6.2 命令泵

创建 `RemoteBridge`（桥本身不创建系统资源，测试可同样驱动命令通道）；服务线程只在配置启用时启动：

1. 建 `mpsc::channel<(Command, oneshot::Sender<Result<Value, RemoteError>>)>`；
2. 实现 `RemoteHost`：`dispatch` 将命令与 oneshot 发送端写入通道，await oneshot；
3. `cx.spawn` 泵任务循环：`recv()` → `this.update(cx, |haloeth, cx| apply_command(haloeth, command, cx))` → 结果经 oneshot 回传 → 按命令类别发布 `state.changed` / `sessions.changed`。

`apply_command` 是既有动作的薄封装，全部复用现有入口（动作覆盖矩阵见 11.2）：

| 命令                                         | 复用入口                                                                                           |
| -------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| `message.send`                               | 从 composer 订阅体抽出的 `Haloeth::submit_message`（见 6.4）                                       |
| `run.cancel`                                 | `Session::cancel`                                                                                  |
| `approval.resolve`                           | `Session::resolve_approval` + `save_active`（与 `ConversationEvent::ApprovalDecision` 订阅体一致） |
| `session.new` / `open` / `delete` / `rename` | `Haloeth::create_session` / `open_session` / `delete_session` / `rename_session`                   |
| `model.select`                               | `Session::select_model`                                                                            |
| `approval.mode`                              | `Haloeth::set_approval_mode`                                                                       |
| `feature.set`                                | `Haloeth::toggle_feature` 的参数化形态                                                             |
| `grant.add` / `grant.remove`                 | `Session::set_grants`（替换式更新）                                                                |
| `state.get`                                  | 主线程实时读取（state DTO 组装，见 6.4）                                                           |

忙语义与桌面一致：`message.send` 在运行中返回 `busy` 错误并回显文本（客户端据此恢复输入框，等价于桌面把文本放回 composer）；`session.open` / `delete` 在运行中同样拒绝。

### 6.3 事件 tap（通知的来源）

运行期通知来自 `Session::submit` 的事件泵——这是唯一逐事件看到 `Event` 的主线程位置。`Session` 增加一个可选 tap（`set_remote_tap`，由 `Haloeth` 构造时注入，默认 `None`，不产生通知；注入模式同 `set_model_builder` 的可选注入）：

- 事件投影：泵循环内在 `apply` / `handle_effect` 之后调用 `tap.publish(&event, …)`。映射是纯函数：`Delta → message.delta`、`Step → message.completed`（文本由 `render_step` 计算，同一函数两个消费者）、`Activity` 各变体 → 对应通知（覆盖矩阵见 11.1）。
- 状态投影：`Status` 变更收敛到单一 `set_status` 辅助方法后统一发布 `status.changed`（现有赋值散布在 `submit` 入口、`handle_effect`、`finish` / `fail`、`cancel`；收敛后发布点唯一）。
- 用量投影：`Event::Step` 的步骤用量、`Activity::ToolUsage` 与 `finish` 的校准结果统一发布 `usage.changed`（总量随步骤累计，分模型在运行结束校准，与状态栏胶囊同源）。
- 快照投影：`Haloeth::activate` 推 `transcript.reset`（启动时的初始状态由连接快照提供）；`refresh_tray` 内顺带发布 `sessions.changed`（`refresh_tray` 的全部调用点与会话列表变化的时机一致：保存、切换、新建、重命名、删除）。
- 序号分配：通知在发布时由桥分配单调递增的 `seq`（服务进程生命周期内）；连接建立时合成的 `transcript.reset` 携带计数器高水位（不消耗计数器值），会话切换时推送的 `transcript.reset` 按普通通知分配序号。客户端据此重置游标（协议规范第 4 节）。
- 增量续接：桥同时把每条通知写入历史环形缓冲（容量 = 广播容量）；`session.resume` 由写任务负责回放，保证回放帧与实时通知同序，重复帧由客户端游标丢弃。

transcript.reset 的 entries 取自 `Conversation::snapshot(cx)` 的实时快照而非 `SessionFile`：重连可能发生在运行中，落盘文件滞后于流式中的会话；同时携带 `streaming` 字段（`Conversation` 的 `streaming` 消息与已累积 `source`），客户端可完整接续正在输出的消息。

### 6.4 既有代码的单点化与 DTO 映射

两处小重构用于消除重复，属于本方案的组成部分：

1. 抽取 `Haloeth::submit_message(text, cx)`：composer 订阅回调与远程 `message.send` 共用同一入口（busy 检查、用户消息写入、`Session::submit`、`save_active`）。发布 `message.appended` 的位置也在这里，发送方与其它客户端收到同一广播，客户端无需本地回显。
2. `Status` 赋值收敛为 `Session::set_status`（见 6.3）。

DTO 与内部类型的关系：协议类型（`remote::proto::dto`）是**线上稳定契约**，与内部类型分离——内部类型（`TranscriptEntry` 等）随界面演进重构，线上类型受兼容性规则约束（未知字段忽略、未知操作结构化报错）。两者之间的映射集中在 `haloeth/src/remote.rs` 一处（`From` / `Into` 实现），等价于持久化层已有的 `ActivityCard::record()` 投影，不构成逻辑重复。

## 7. 桌面端界面与配置

配置（`config.toml` 新段，写入既有 `Config`）：

```toml
[remote]
enabled = false        # 默认关闭；开启时才启动服务线程
port = 8737            # 监听端口
bind = "0.0.0.0"       # 绑定地址；可改成具体网卡或 127.0.0.1
tls = false            # 自签 TLS；客户端按配对信息中的指纹钉扎

[[remote.devices]]     # 已配对设备；令牌只存哈希
id = "uuid"
name = "iPhone"
token_hash = "sha256-hex"
added_at = 1730000000
last_seen = 1730000000
```

偏好窗口新增「远程」分页（沿用 `settings/` 表单规范：纵向字段、`field` / `input_frame`、2px 边框）：

- 启用开关与端口 / 绑定地址（改动即重启服务，与偏好页「即时生效」一致）。
- 配对区：生成新设备令牌 → 渲染二维码（`qrcode` crate 生成模块矩阵，默认特性无传递依赖；GPUI `path()` 把模块绘制为矩形集）+ 手动复制串。
- 设备列表：名称、最近在线时间、撤销按钮（`open_alert_dialog` 确认）。
- 附件：上传与拖放的文件都复制到应用数据目录 `Haloeth/attachments/<会话>`；该目录获得会话级只读授权，消息以 `[附件] <路径>` 行引用，会话删除时清理附件。
- 状态行：当前监听地址、已连接数；Windows 首次绑定会触发防火墙授权弹窗，页面附说明文案。

主界面不加常驻入口：远程是跨会话能力，归属偏好页（与「主界面只放当前对话快速调整项」的既有分工一致）；托盘菜单不加远程项。

## 8. 测试策略

`remote` crate（无 UI，全部可离线运行）：

- `wire.rs`：每类消息的黄金 JSON 快照。字段改名、枚举值变化均会被捕获，作为线上兼容性的回归测试。
- `e2e.rs`：临时端口起服务（内存宿主）+ `remote::client` 回环连接：hello 鉴权、命令分发、通知扇出、慢客户端断开（4429）、令牌错误（4403）逐项断言。

`haloeth` 桥（既有 UI 测试设施）：

- `#[gpui_kit::test]` + `open_shell`（临时会话目录 + 空配置，`persist_config = false` 不启动真实服务，直接驱动桥的命令通道）。
- `ScriptedModel` + `Session::set_model_builder`：远程 `message.send` → 断言对话区出现用户消息与 scripted 回复、通知流出现 `message.appended` / `message.delta` / `message.completed` / `status.changed`。
- 审批回环：`ApprovalMode::Manual` 下远程收到 `approval.requested` → `approval.resolve` → `Activity::ApprovalResolved` 回流到通知流与对话卡片；双端同时 resolve 只生效一次。
- 重连接续：运行中触发 `transcript.reset`，断言 `streaming` 字段与后续 `message.delta` 无缝续接。
- `src/app/remote_tests.rs`：命令泵、通知流、审批回环、会话切换快照、功能开关 / 授权 / 模型选择与偏好页设备管理。

每阶段收尾执行全量验证：`cargo test --workspace`、`cargo clippy --workspace --all-targets`、`cargo fmt --all --check`。

## 9. 实施阶段

| 阶段 | 内容                                                                                                                                        | 交付判据                                             |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------- |
| 一   | `remote` crate：proto 类型、`RemoteHost`、服务与连接状态机、参考客户端、wire/e2e 测试                                                       | 全部测试通过；协议规范与实现逐条对应                 |
| 二   | `haloeth` 桥：命令泵、事件 tap、`submit_message` 抽取、配置与偏好页（含二维码、设备管理）                                                   | UI 测试覆盖命令面与通知流；README 行为章节更新       |
| 三   | TLS：自签证书（`rcgen` + `rustls`）与二维码指纹钉扎；证书存于应用数据目录；Origin 校验、令牌限速与防火墙说明已落地 | TLS 回环测试通过；明文模式给出警告提示              |
| 四   | 参考客户端：`remote::client`（含指纹钉扎）、`examples/remote-cli.rs` 与内置 Web 客户端（服务端 `/` 直接托管） | 第三方依据协议规范可独立接通；回环测试覆盖页面投递 |
| 五   | 附件上传：`POST /v1/attachments`、`message.send` 附件标识解析与 `[附件]` 引用、会话级只读授权；桌面拖放复制到同一目录 | 上传 → 发送 → 引用全链路测试通过；桌面与远程行为一致 |
| 六   | 增量续接：`session.resume`、宿主历史环形缓冲、写任务有序回放与快照回退               | 补发与回退测试通过                                   |

文档随阶段更新：README「特性 / 行为」与 CLAUDE.md「架构要点」在对应阶段一并修订。

## 10. 风险与对策

| 风险                                         | 对策                                                                 |
| -------------------------------------------- | -------------------------------------------------------------------- |
| GPUI 嵌套实体更新 panic                      | 泵任务顶层 `this.update`，逐命令处理；偏好页不在渲染期间读取 Haloeth，状态经缓存与 `window.defer` 刷新 |
| 双 Tokio 运行时并存（Session 运行时 + 远程） | 各自线程、无共享非 `Send` 状态；`Background` 线程已是先例            |
| 审批双端竞态                                 | `ApprovalHandle` 单次唤醒；第二次 resolve 为幂等 no-op，响应仍 200   |
| 运行中重连丢流式文本                         | `transcript.reset` 取实时快照并携带 `streaming` 缓冲                 |
| 广播积压 / 慢客户端                          | 有界队列 + 4429 断开 + 全量重同步恢复                                |
| release 体积（axum 栈）                      | 实现期测量；超预算则改用 `tokio-tungstenite` 最小服务（3.2 备选）    |
| 局域网明文嗅探                               | 默认明文可改为自签 TLS（`[remote].tls`）+ 指纹钉扎；跨网推荐隧道 |
| Windows 防火墙首次弹窗                       | 偏好页说明文案；绑定地址可配置规避                                   |
| 令牌泄露                                     | 每设备独立、可撤销、可轮换；只存哈希                                 |

## 11. 交叉核对

### 11.1 事件覆盖矩阵

代理内核 `Event` / `Activity` 的每个变体（`agent/src/runtime.rs`）必须有明确的线上归宿；逐行核对桌面的消费点（`apply` / `apply_activity` / `handle_effect`），确保「桌面显示什么、远端显示什么」一致：

| 内核事件                      | 桌面行为                                                | 线上通知                          |
| ----------------------------- | ------------------------------------------------------- | --------------------------------- |
| `Event::Delta`                | `Conversation::stream`                                  | `message.delta`                   |
| `Event::Step`                 | `end_stream` + 可选 `push`（`render_step`）             | `message.completed`（含渲染文本） |
| `Activity::Thinking`          | 仅状态栏活动文案                                        | `status.changed`（activity 字段） |
| `Activity::ReasoningDelta`    | `reasoning`（合并思考卡片）                             | `reasoning.delta`                 |
| `Activity::ToolStarted`       | `end_reasoning` + `tool_activity`（新卡片）             | `tool.started`                    |
| `Activity::ToolFinished`      | `finish_activity`（状态 / 预览 / 耗时）                 | `tool.finished`                   |
| `Activity::ApprovalRequested` | `approval_requested`（manual ? Pending : Running 卡片） | `approval.requested`              |
| `Activity::ApprovalResolved`  | `approval_resolved`（卡片终态）                         | `approval.resolved`               |
| `Activity::SkillLoaded`       | 仅状态栏活动文案                                        | `status.changed`                  |
| `Activity::PlanStarted`       | 仅状态栏活动文案                                        | `status.changed`                  |
| `Activity::PlanFinished`      | `plan`（计划卡片）                                      | `plan.appended`                   |
| `Activity::Images`            | `push_images`                                           | `images.appended`                 |
| `Activity::ToolUsage`         | 状态栏用量累计                                          | `usage.changed`                   |
| `Session::Status` 任意迁移    | 状态栏 + 自动保存触发                                   | `status.changed`                  |
| 会话切换 / 恢复               | `Conversation::restore`                                 | `transcript.reset`                |
| 会话列表变化（保存 / 增删改） | `refresh_tray`                                          | `sessions.changed`                |

全部变体均有归属；「仅状态栏」类事件按既有行为降级为 `status.changed`，不为远端新增展示形态。

### 11.2 动作覆盖矩阵

桌面主窗口 / 托盘的全部用户动作（CLAUDE.md「界面与输入」核对）与命令面一一对应：

| 桌面动作                        | 命令 / 通道                                                   | 备注                                                                                |
| ------------------------------- | ------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| Composer 发送                   | `message.send`                                                | busy 时回显文本，与桌面放回输入框同语义                                             |
| 停止运行                        | `run.cancel`                                                  | 同时拒绝挂起审批，同 `Session::cancel`                                              |
| 审批卡片三键                    | `approval.resolve`                                            | 幂等                                                                                |
| 会话新建 / 切换 / 删除 / 重命名 | `session.new/open/delete/rename`                              | 新建 / 切换 / 删除在运行中拒绝，重命名不受限，均与桌面守卫一致                      |
| 模型选择器（文本→文本过滤）     | `model.select` + `state.models`                               | 过滤在服务端完成（`Config::models_with`），客户端不下发目录                         |
| 审核模式四档                    | `approval.mode`                                               | 全局配置写入                                                                        |
| 目录授权增删                    | `grant.add` / `grant.remove`                                  | 会话级，随 `sessions.changed` / `state.changed` 同步                                |
| 工具 / MCP / Skill 开关         | `feature.set`                                                 | 键名与 `SessionFile.features` 一致                                                  |
| 附件上传                      | `POST /v1/attachments` + `message.send` 的 `attachments` 参数；桌面拖放 | 两端都复制到会话附件目录并获得只读授权；消息以 `[附件]` 行引用 |
| 偏好设置、语音、图片查看器      | 不开放（非目标）                                              | 图片查看由客户端自行渲染（资源端点）                                                |

### 11.3 平台与并发约束核对

- 嵌套更新：命令泵顶层消费，通知发布在实体更新内部但只做同步 `broadcast::send`（无二次 update）。
- 双运行时：远程运行时仅承载服务与转发，不触碰 `agent`；会话运行时保持不变。
- 审批竞态：`ApprovalHandle::resolve` 查找等待者表，无等待者即 no-op；对话卡片状态由首个 `ApprovalResolved` 事件定型。
- 通知顺序：单发送者（主线程泵）经 broadcast 保序；跨连接无需全序。
- 发布开销：无连接时 `broadcast::send` 仅执行一次加锁与接收者计数检查即返回错误，tap 的热路径开销为常量时间。
- 持久化频率：不因远程改变——落盘点仍是既有状态迁移触发（`save_active`），通知不写盘。
- 体积预算：阶段一完成后实测增量，超预算启用 3.2 备选。
- 配置明文边界：令牌只存哈希，不新增明文秘密。

## 12. 边界声明（随 README「行为」发布）

- 远程默认关闭；开启即监听局域网，默认明文、可启用自签 TLS（客户端按指纹钉扎），安全性依赖令牌与网络边界，跨网访问建议隧道。
- 远程命令继承当前审核模式，不构成审核绕过。
- 语音、偏好编辑、图片查看器交互为设备本地能力，不在远程协议内。
- 协议 v1 与配置格式仍可能调整（对齐「当前版本 0.2.0」的既有声明）。
