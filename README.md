# Haloeth

Haloeth 是一个用 Rust 与 GPUI 编写的轻量桌面智能体。它把对话、工具调用、审核与语音能力放在同一个窗口中，并将界面与代理内核分离，便于各自演进。

## 特性

- **独立的代理内核**：`agent` crate 只处理消息、工具、模型与编排循环，不感知界面；`haloeth` 负责界面、会话存储与运行编排。
- **对话式执行**：思考过程、工具调用、审批与计划以活动卡片呈现，最终答复按 Markdown 流式写入。
- **四档审核**：手动审核、规则自动、模型自动与无审核，默认规则自动。目录授权跟随会话；调用超出授权范围时不会被直接拒绝，规则模式转交用户确认，模型模式交给审核模型（未配置审核模型时回退用户确认）。
- **多提供商与多协议**：内置 OpenAI Chat 与 Anthropic Messages 两种协议，可按提供商附加请求头与额外请求体、选择工具调用策略；提供 OpenAI、Anthropic、DeepSeek、Gemini、OpenRouter、硅基流动、百炼、智谱、Moonshot 与本地 Ollama / vLLM 预设。
- **按模态筛选模型**：每个模型声明输入与输出模态，对话、审核、语音与生成工具各自只显示匹配的模型。
- **内置工具**：默认提供 `current_time`、`fetch_url`、`read_file`、`write_file`、`list_dir`、`shell` 与 `show_image`，代理循环还会注入用于收尾的 `final_answer`；配置搜索接口或生成模型后，还会注册 `web_search`、`generate_image`、`generate_video` 与 `generate_music`；生成类工具默认隐藏且停用，可在工具页启用。
- **MCP 与 Skill**：MCP 支持 stdio 与 Streamable HTTP；Skill 是包含 `SKILL.md` 的目录包，系统提示只列元数据，模型按需调用 `load_skill`。
- **语音输入与播报**：支持云端转写或系统听写，以及 Windows SAPI 或云端 TTS；可配置按住说话快捷键。
- **桌面集成**：托盘、后台运行、多会话管理、图片查看器与会话级功能开关。
- **偏好与界面**：跨会话配置在独立窗口中即时生效，支持 JSON 导出与导入；支持跟随系统、浅色与深色配色，统一 2px 描边，输入区高度可拖动，用量按模型分列。

## 架构

工作区包含两个 crate：

| crate     | 职责                                                   |
| --------- | ------------------------------------------------------ |
| `haloeth` | GPUI 界面、会话文件、配置、语音、后台线程与运行编排    |
| `agent`   | 消息模型、工具契约、模型后端、审核、记忆、MCP 与 Skill |

一次运行的链路如下：

```text
Composer 提交 → Conversation 写入用户消息 → Session::submit
  → 构建模型 → prepare_tools（内置 + MCP + Skill）→ Agent::run
  → Event → UI 执行器 → 对话与状态栏
  → finish：快照写回会话 → 状态变化触发自动保存
```

代理内核不负责持久化：会话文件由前端读写，运行上下文通过 `ContextSnapshot` 恢复。系统提示在每次运行时重建，内容包含当前工具、Skill 元数据与会话授权的目录。

## 开始使用

1. 运行 `cargo run`，在「偏好设置 → 模型」中添加提供商与 API 密钥，并启用至少一个模型。
2. 在输入框上方选择模型，按 Enter 发送（可在偏好设置中改为 Ctrl+Enter，macOS 为 ⌘+Enter）。
3. 需要审批时，按提示选择「允许一次」「始终允许」或「拒绝」；需要访问文件时，通过工具栏的目录授权入口添加路径。目录授权与「始终允许」规则随当前会话保存，审核模式写入全局配置。

## 行为

Haloeth 的数据位置如下：

- 配置：Windows 为 `%APPDATA%\Haloeth\config.toml`；其它平台为 `$XDG_CONFIG_HOME/Haloeth/config.toml`，缺省时回退到 `~/.config/haloeth/config.toml`。
- 会话：Windows 为 `%LOCALAPPDATA%\Haloeth\sessions`；其它平台为系统本地数据目录下的 `Haloeth/sessions`。可在偏好设置中改为自定义目录。
- 日志：Windows 为 `%APPDATA%\Haloeth\logs\haloeth.log`；其它平台为系统数据目录下的 `Haloeth/logs/haloeth.log`。
- Skill：默认读取 `~/.agents/skills`；旧版本配置目录下的 `skills` 子目录仍会读取。

需要注意：

- 配置、会话文件与 API 密钥均为明文，项目不提供加密。
- Shell 与文件操作以当前用户身份运行，项目不提供沙箱；stdio 方式的 MCP 服务器也是本地程序，添加即视为信任。
- `无审核` 模式会放行全部调用；未声明影响的工具按未知影响处理。非 Windows 平台的可用性尚未验证。
- 审核模式、审核模型与提供商定义是全局的；会话可以保存自己的模型选择、目录授权、始终允许规则与功能开关。
- 托盘、全局快捷键与语音播报目前仅在 Windows 上可用。语音识别可选云端模型（跨平台）或系统听写（Windows 与 macOS）。
- MCP 只支持 tools，不支持 resources、prompts 与 OAuth。
- 当前版本为 0.2.0，配置与会话格式仍可能调整。

## 开发

需要 Rust 2024 edition（Rust 1.85 及以后）与可用的图形环境。当前以 Windows 为主要目标平台。

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all --check
cargo run
```

完成一个阶段前，应确保前三条命令全部通过。Release 构建以体积为先：`opt-level = "z"`、fat LTO、单一编译单元、`panic = "abort"` 与符号剥离，并通过 `windows_subsystem = "windows"` 隐藏控制台；Debug 构建保留控制台以便查看日志。

真实模型密钥通过 `HALOETH_TEST_KEYS_FILE` 传给被忽略的实网测试，不写入仓库。

## 参与贡献

欢迎参与改进。功能建议、问题反馈与代码改动都可以通过 Issue 或 Pull Request 提出；参与前请阅读仓库的 `CODE_OF_CONDUCT.md`。

1. 复刻（Fork）本仓库
2. 创建功能分支（`git checkout -b feature/your-feature`）
3. 提交改动（`git commit -m 'Add your feature'`）
4. 推送分支（`git push origin feature/your-feature`）
5. 发起 Pull Request

如果只是想提出建议，也可以直接创建带 `enhancement` 标签的 Issue。

### 贡献者

<a href="https://github.com/TrifingZW/haloeth/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=TrifingZW/haloeth" alt="贡献者" />
</a>

## 许可

本项目采用双许可，可在 MIT 与 Apache-2.0 之间选择。仓库根目录的 [`LICENSE-MIT`](LICENSE-MIT) 与 [`LICENSE-APACHE`](LICENSE-APACHE) 分别为对应许可文本；`Cargo.toml` 中的包声明为 `MIT OR Apache-2.0`。

## 联系

维护者：TrifingZW

- 邮箱：<TrifingZW@gmail.com>
- 仓库：[https://github.com/TrifingZW/haloeth](https://github.com/TrifingZW/haloeth)
- 问题反馈：优先使用仓库的 Issues
