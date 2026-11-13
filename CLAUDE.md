# CLAUDE.md

## 项目概览

Haloeth 是一个 GPUI 桌面智能体，工作区包含两个 crate：

- `haloeth`（根 crate，二进制）：界面、会话存储、配置、运行编排。
- `agent`（`agent/`，库）：消息模型、工具契约、模型后端、审核、记忆、MCP、Skill。

代理内核不感知文件路径与会话文件：它只收 `ContextSnapshot`，由前端负责持久化。

## 常用命令

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all --check
cargo run
```

每个阶段完成前必须让上面三条全部通过。

## 代码约定

- 文档注释与用户可见文案用中文；**面向模型的文本**（提示词、工具描述、错误回灌观测）用英文。
- 所有边框统一 2px：`.border_2()`、`.border_t_2()`、`.border_b_2()`；组件默认的 1px 边框要在调用处覆盖。
- 可测试元素加 `.test_support()` 与稳定 `ElementId`；`window.find` / `within` 依赖它。
- 面板/对话框沿用 `models.rs` 的模式：草稿配置 + 保存后应用；确认动作用 `open_alert_dialog`。
- 颜色语义集中在 `src/ui.rs`：`Tone`、`tinted`、`chip`、`cancel_button`。启用用绿色、停用/取消用淡红、等待用黄色、当前项用主色。
- 主界面只放当前对话需要快速调整的东西（审核模式、目录授权、工具/MCP/Skill 开关）；跨会话配置放设置。
- 代理内核的公开类型尽量保持无 UI 依赖；UI 相关类型（`TranscriptEntry`、`ActivityRecord`）放在 `src/conversation.rs`。

## 架构要点

### 运行链路

```
Composer 提交 → Conversation 写用户消息 → Session::submit
  → Tokio 运行时任务：构建模型 → prepare_tools（内置 + MCP）→ Agent::new
  → restore_context → Agent::run
  → Event 通道回到 UI 执行器 → Conversation / 状态栏
  → finish：快照写回 Session → 状态变化触发自动保存
```

- `Event`：`Delta`（文本）、`Step`（整步完成，持久化与用量）、`Activity`（思考/工具/审批/计划）。
- 审批门与运行任务共享事件发送端；`Agent::close_events()` 在运行结束释放发送端，消费侧才能看到通道关闭。
- 工具调用顺序固定为：查找 → 校验参数 → 审核 → 执行。参数不合法不产生审批请求。
- 可恢复错误作为观测回灌；`Denied` / `Cancelled` 不附重试提示。

### 存储边界

- `agent::persist::ContextSnapshot` 只含步骤；系统提示每次重建。
- `src/session_store.rs` 负责 `SessionFile` 的原子读写；`Conversation` 负责显示记录的导出与恢复。
- 显示记录与执行上下文分开保存，恢复时互不补写，避免重复消息或重复上下文。

### 审核

- `agent/src/approval.rs`：四档 `ApprovalMode`、`PolicyGate`、路径授权、Shell 分类、审核模型。
- 路径比较：`dunce` 规范化、Windows 忽略大小写、`..` 越界拒绝。
- 规则拿不准、MCP 未知工具、无授权目录 → 升级人工或审核模型；审核模型失败一律拒绝。
- 目录授权与「始终允许」规则跟随会话；审核模式与审核模型是全局配置。

### 模型

- `GenerateRequest` 默认要求调用工具，但真正发送的 `tool_choice` 由 `HttpModelOptions.tool_choice` 决定：`Auto`（默认）/ `Required` / `Omit`。
- 思考型模型（DeepSeek `deepseek-flash`、`deepseek-v4-pro`）只接受 `auto` 或省略；发送 `required` 会返回 400。
- 每个提供商在配置里保存自己的策略，可在「模型」对话框切换。

### MCP

- 协议与传输全部由 `rmcp` 处理，项目代码不做 JSON-RPC。
- stdio 用 `TokioChildProcess::builder(..).stderr(piped())`（`new` 会覆盖 stderr）。
- Streamable HTTP 用 `StreamableHttpClientTransport::from_config`；默认客户端只跟随同源重定向。
- 工具名 `mcp__{server}__{tool}` 会清洗并截断；参数校验交给服务器（`Validation::None`），审核按 `ToolEffect::Unknown` 处理。
- 连接失败只让该服务器的工具不可用。

### Skill

- 目录包：`SKILL.md` 的 YAML frontmatter（`name` / `description` / `when_to_use`）+ 正文。
- 系统提示只列元数据；`load_skill` 返回正文与资源清单。
- Skill 目录隐式只读授权。

## 测试

- 纯逻辑：`#[test]`；异步：`#[tokio::test]`；UI：`#[gpui_kit::test]`。
- UI 测试用 `open_shell`（临时会话目录 + 空配置），通过稳定的 ElementId 查找。
- 需要真实运行代理时，用 `ScriptedModel` + `Session::set_model_builder` 离线完成。
- 跨 Tokio 运行时的事件测试先调用 `cx.executor().allow_parking()`。
- MCP 测试用 `rmcp` 的 server feature 与 `tokio::io::duplex` 进程内对端。

## 已知边界

- 不做代码即动作 / 沙箱执行器。
- MCP 只支持 tools，不支持 resources / prompts / OAuth。
- 不支持第三方搜索 API；搜索依赖提供商接口。
- 会话文件与配置为明文，未做加密。
