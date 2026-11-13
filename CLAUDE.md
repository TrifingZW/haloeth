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
- Windows 图标由 `build.rs` + `embed-resource` 写入 `assets/icon.ico`。
- 所有边框统一 2px：`.border_2()`、`.border_t_2()`、`.border_b_2()`；组件默认的 1px 边框要在调用处覆盖。
- 可测试元素加 `.test_support()` 与稳定 `ElementId`；`window.find` / `within` 依赖它。
- 偏好窗口（`src/preferences.rs`）即时生效：控件改动直接写配置，文本框通过订阅触发保存；旧的设置 / 模型对话框仅保留给测试与内部入口。
- 确认动作用 `open_alert_dialog`；Skill 删除使用 `agent::skill::delete_skill`（要求目录里存在 `SKILL.md`）。
- 颜色语义集中在 `src/ui.rs`：`Tone`、`tinted`、`chip`、`cancel_button`。启用用绿色、停用/取消用淡红、等待用黄色、当前项用主色。
- 表单小部件同样在 `src/ui.rs`：`field`（标签 + 控件）、`input_frame`（2px 外框输入框）、`pick_folder`（系统目录选择器）。新面板直接复用，不要各自再写一份。
- 主界面只放当前对话需要快速调整的东西（审核模式、目录授权、工具/MCP/Skill 开关、语音按钮）；跨会话配置放「偏好设置」独立窗口。
- 左下角「⋯」是 `Popover` 控制的常驻面板：开关项点击后不收起，点击面板外才关闭。不要改回点击即关的 `dropdown_menu`。
- 功能启停分两层：`Config.feature_prefs`（显示 / 默认启用，隐藏即不加载）+ `SessionFile.features`（会话级覆盖）。不要再用「空列表 = 全部启用」的哨兵语义。
- 新建 MCP / Skill 使用独立对话框（`src/mcp_form.rs`、`src/skill_form.rs`），字段纵向排列；对话框只写设置草稿，由设置面板统一保存。
- 代理内核的公开类型尽量保持无 UI 依赖；UI 相关类型（`TranscriptEntry`、`ActivityRecord`）放在 `src/conversation.rs`。

## 架构要点

### 界面与输入

- 配色：`ui::apply_theme` 的 `System` 分支调用 `Theme::sync_system_appearance`，必须把窗口传进去；`None` 会退回应用级外观，绝不能写死浅色。
- 发送键：`UiConfig.send_key`（`Enter` / `SecondaryEnter`）。Composer 用 `EditorState::set_submit_on_enter` 切换默认行为，设置保存后由 `Haloeth::sync_input_preferences` 同步。
- 输入区：`UiConfig.line_numbers` 控制行号（默认关闭）；Composer 顶部的拖动条调整高度。当前会话的审核 / 目录 / 模型 / 语音按钮在输入框上方的工具条，底部状态栏只放状态、用量与发送。
- 思考过程是 `ActivityCard` 里标题为「思考过程」的卡片：灰色斜体、正文字体，与最终答复（默认字体与颜色）区分；这类卡片不显示状态文字与展开按钮。
- 用量：`Session::usage_by_model` 按运行模型累计；状态栏 Token 胶囊的 tooltip 列出各模型用量。总量 = 上下文步骤用量 + `extra_usage`（生成类工具，跨运行累计；加载会话时用 `file.usage − steps_usage` 反推）。
- Release 构建用 `windows_subsystem = "windows"` 去掉控制台；Debug 保留控制台查看日志。

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
- 系统提示包含 `## File permissions` 段落：会话授权的目录随每次运行传入（`Agent::with_workspace`）。没有授权时告诉模型「没有固定工作区，按用户需求判断，写入前询问」；有授权时说明这些目录优先可用，但以用户要求为准。
- `src/session_store.rs` 负责 `SessionFile` 的原子读写；`Conversation` 负责显示记录的导出与恢复。
- 显示记录与执行上下文分开保存，恢复时互不补写，避免重复消息或重复上下文。

### 审核

- `agent/src/approval.rs`：四档 `ApprovalMode`、`PolicyGate`、路径授权、Shell 分类、审核模型。
- 路径比较：`dunce` 规范化、Windows 忽略大小写。
- 授权目录之外不是硬拒绝：规则模式升级为人工审批，模型模式交给审核模型（`Escalation::Ask`）；只有参数缺失、无法解析这类情况才直接拒绝。
- 审核模型通过 `ApprovalGate::set_context` 拿到最近 16 条文本消息（代理每步更新），系统提示要求它结合用户意图判断；没有可用审核模型时退回人工，而不是静默全拒。
- 审核回答允许包裹说明文字（`parse_review` 会找 JSON 对象）；无法解析才是拒绝。
- 目录授权与「始终允许」规则跟随会话；审核模式与审核模型是全局配置。

### 模型

- `GenerateRequest` 默认要求调用工具，但真正发送的 `tool_choice` 由 `HttpModelOptions.tool_choice` 决定：`Auto`（默认）/ `Required` / `Omit`。
- 没有工具调用但有文本时，`action_step` 把这段文本当作最终答案（很多模型在 `auto` 下直接作答）；只有空回复才记 `Parsing` 错误。
- 思考型模型（DeepSeek `deepseek-flash`、`deepseek-v4-pro`）只接受 `auto` 或省略；发送 `required` 会返回 400。
- 每个提供商保存 `protocol`（`openai-chat` / `anthropic-messages`）、自定义请求头、额外请求体与 `max_tokens`；`agent::openai::build_model` 按协议构造模型，会话与审核器都走它。
- 每个模型条目带输入 / 输出模态（`text` / `image` / `audio` / `video`）；所有模型选择器用 `Config::models_with` 过滤：对话与审核 = 文本→文本，ASR = 音频→文本，TTS = 文本→音频，生成工具 = 文本→图像/视频/音频。
- 模型请求前按 `HttpModelOptions.input_modalities` 裁剪消息；工具结果里的媒体块在 OpenAI 投影中拆到随后的用户消息，Anthropic 放进 `tool_result`。
- 内置提供商预设见 `src/config.rs::PROVIDER_PRESETS`。

### 语音与生成工具（前端）

- 语音识别用 `gpui-kit` 的 `speech` feature（`SpeechState` + `SelectableMicrophone`，支持设备选择），`src/audio/recognizer.rs` 实现云端转写或系统听写；百炼自动走原生 `multimodal-generation` + Base64 音频。
- 语音播报在专用线程执行：Windows SAPI（`windows` crate）或云端语音 + `PlaySound`；百炼自动使用原生 Qwen TTS 并下载 WAV。`TtsService` 只接收「朗读 / 停止」命令；偏好页提供麦克风测试与试听。
- 生成工具在 `agent/src/tools/media.rs`：绘图默认走 OpenAI 兼容 `images/generations`；百炼（DashScope）自动切换为原生异步任务（`X-DashScope-Async` + `/api/v1/tasks/{{id}}`）；视频 / 音乐使用可配置的「提交 → 轮询 → 下载」适配器，`src/media_form.rs` 提供高级设置。产物路径由模型参数给出并走写审核。
- 生成工具默认隐藏且默认禁用；未配置可用模型时工具页只显示「未配置模型」，点击跳到模型页。
- 后台能力在 `src/background.rs`：独立线程跑隐藏窗口 + 托盘 + 低级键盘钩子（`WH_KEYBOARD_LL`，因为 `RegisterHotKey` 收不到松开）；事件经通道由界面线程的轮询任务取出。
- **事件处理必须在实体更新之外完成**：在 `this.update(...)` 里再 `this.update(...)` 会 panic（`cannot update ... while it is already being updated`），托盘「偏好设置」就是这样崩的。轮询任务先 `take_background_events()`，再在顶层用 `cx.update_window` / `this.update` 分别处理每个事件。
- 关闭主窗口时按 `UiConfig.run_in_background` 选择「最小化到后台」或「退出」：用 `Window::on_window_should_close` 拦截平台 `WM_CLOSE`（`TitleBar::on_close_window` 只在 Linux 生效）并返回 `false`，否则 GPUI 在最后一个窗口关闭后直接退出。
- 托盘菜单用 `TPM_RETURNCMD` 取回命令号：未加该标志时 `TrackPopupMenu` 只发 `WM_COMMAND`，隐藏窗口不处理它，菜单项点了没有反应。
- 托盘菜单里的会话列表由 `Haloeth::refresh_tray` 推给后台线程（`TrayState`）；「当前对话」是子菜单，勾选当前会话，点击其它会话即切换。
- 按住说话：`TalkStart` 置 `auto_submit_speech`，`SpeechEvent::Final` 经 `Haloeth::apply_speech_result` 直接提交；手动点麦克风只追加到输入框。快捷键在偏好里点「录制」按下组合键写入（`SettingsForm::capture_hotkey`）。
- 日志在 `src/logging.rs`，写到应用数据目录的 `Haloeth/logs/haloeth.log`；`logging::init()` 在 `main` 里调用一次。
- `show_image` 把本地图片显示到对话；`Activity::Images` 携带本地路径，`src/image_viewer.rs` 用 `RenderImage` 渲染，支持旋转 / 镜像 / 滚轮缩放 / 左键拖动，窗口与图片默认居中。
- 多模态实测：DeepSeek / 百炼 / 智谱的文本与工具循环、百炼视觉输入、智谱 CogView 与百炼万相图片生成；真实密钥只通过 `HALOETH_TEST_KEYS_FILE` 传入 ignored 测试，不写入仓库。

### MCP

- 协议与传输全部由 `rmcp` 处理，项目代码不做 JSON-RPC。
- stdio 用 `TokioChildProcess::builder(..).stderr(piped())`（`new` 会覆盖 stderr）。
- Streamable HTTP 用 `StreamableHttpClientTransport::from_config`；默认客户端只跟随同源重定向。
- 工具名 `mcp__{server}__{tool}` 会清洗并截断；参数校验交给服务器（`Validation::None`），审核按 `ToolEffect::Unknown` 处理。
- 连接失败只让该服务器的工具不可用。

### Skill

- 目录包：`SKILL.md` 的 YAML frontmatter（`name` / `description` / `when_to_use`）+ 正文。
- 系统提示只列元数据；`load_skill` 返回正文与资源清单。
- Skill 目录隐式只读授权；默认目录是 `~/.agents/skills`，旧的 `%APPDATA%/Haloeth/skills` 仍会读取。

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
- 视频 / 音乐生成没有统一标准，只提供通用异步任务适配器。
- 语音播报目前仅 Windows 可用；识别云端部分跨平台。
- 会话文件与配置为明文，未做加密。
