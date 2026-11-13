# Haloeth

轻量桌面智能体：随时呼出、完成具体任务。当前版本 0.2.0。界面基于 GPUI，代理内核独立成 `agent` crate。

## 功能

- **对话式代理**：工具调用的「动作—观测」循环，流式输出、思考过程、实时活动时间线。
- **偏好设置窗口**：独立的可缩放窗口，左侧导航可收起为图标，改动即时生效，支持配置导出 / 导入 JSON。
- **输入区**：上方工具条放置审核 / 目录 / 模型 / 语音按钮；输入框可拖动调整高度，可选显示行号。
- **用量**：状态栏显示总 Token，悬停可按模型查看各自用量。
- **主界面快速设置**：左下角「⋯」展开常驻面板，可连续切换工具、MCP、Skill，点击面板外才收起；面板只显示全局「显示」的功能。
- **全局 / 会话开关**：偏好里设置功能是否显示与默认启用；会话内的启停只影响当前会话。
- **多会话**：自动保存、切换、重命名、删除；默认存放在系统数据目录，Windows 可自定义。
- **配色**：跟随系统 / 浅色 / 深色，在设置中切换；状态与开关用颜色区分（绿色启用、淡红停用/取消）。
- **按键偏好**：默认 `Enter` 发送、`Shift+Enter` 换行；也可切换为 `Ctrl+Enter` 发送、`Enter` 换行。
- **工具**：读写文件、列目录、Shell、网页访问、当前时间；提供商搜索接口封装为 `web_search`（DeepSeek `POST /tools/search`）。
- **四档审核**：手动 / 规则自动 / 模型自动 / 无审核；目录可指定只读或读写，授权之外的操作升级给人工或审核模型。
- **文件权限感知**：会话授权的目录会写入系统提示，模型知道自己是否有固定工作区以及每个目录的权限。
- **多协议多模态模型**：每个模型声明输入 / 输出模态（文本、图像、音频、视频），所有模型选择器按用途过滤；支持 OpenAI Chat 与 Anthropic Messages，可自定义请求头与额外请求体。
- **语音**：麦克风转写（云端模型或 Windows 听写，可选输入设备）与回复播报（Windows SAPI 或云端语音）；百炼使用原生 Qwen ASR/TTS，偏好页可直接测试麦克风与试听。
- **后台运行**：关闭主窗口后缩到系统托盘继续运行；托盘菜单可「显示界面 / 当前对话（子菜单切换） / 偏好设置 / 退出」。
- **按住说话**：默认 `Ctrl+V` 全局按住录音，松开后转写并自动发送；快捷键在偏好里点「录制」按下组合键即可设置。
- **通用提示词**：偏好里填写的提示词会附加到每个对话的系统提示。
- **日志**：写入应用数据目录的 `Haloeth/logs/haloeth.log`，偏好里可一键打开目录。
- **生成工具**：AI 绘图（OpenAI 兼容 `images/generations`，百炼自动使用原生异步任务）、视频与音乐（可配置的异步任务适配器，工具页可打开高级设置填写请求体、轮询与结果路径）；已实测智谱 CogView 与百炼万相。
- **图片展示**：AI 可通过 `show_image` 把本地图片直接显示在对话里，点击打开独立查看窗口（旋转、镜像、滚轮缩放、左键拖动平移）。
- **开箱即用预设**：百炼预设会自动配置 `qwen3-asr-flash` / `qwen3-tts-flash` / 万相绘图与视频；智谱、OpenAI 预设也会带上图像或语音模型。
- **MCP**：基于官方 [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk)，支持 stdio 与 Streamable HTTP；在独立对话框中新建。
- **Skill**：Claude 风格目录包（`SKILL.md` + 可选资源），默认读取 `~/.agents/skills`；可新建与删除。

## 构建与运行

需要 Rust 1.85+（edition 2024）。Windows 构建会把 `assets/icon.ico` 写入可执行文件。

```sh
cargo run            # 调试运行，保留控制台（首次启用语音会编译 cpal）
cargo build --release # 发布构建，不附带控制台窗口
cargo test --workspace
cargo clippy --workspace --all-targets
```

## 配置

- 配置文件：Windows 为 `%APPDATA%\Haloeth\config.toml`，macOS / Linux 为对应系统配置目录。
- 会话文件：默认在系统数据目录的 `Haloeth/sessions`；可在「设置 → 会话存储」中修改。
- 环境变量回退：`HALOETH_API_KEY`、`HALOETH_BASE_URL`。

```toml
active_provider = "deepseek"
active_model = "deepseek-flash"

[ui]
theme = "system"         # system | light | dark
send_key = "enter"       # enter | secondary-enter
# default_model = { provider = "deepseek", model = "deepseek-flash" }

[[providers]]
name = "deepseek"
base_url = "https://api.deepseek.com"
api_key = "sk-…"
protocol = "openai-chat" # openai-chat | anthropic-messages
tool_choice = "auto"
headers = { "X-Org" = "team" }
[[providers.models]]
name = "deepseek-flash"
input = ["text"]
output = ["text"]

[approval]
mode = "rules"           # manual | rules | model | off（默认规则自动）
allow_network = false
reviewer.use_current = true

[search]
enabled = true
max_results = 5

# 思考型提供商（如 DeepSeek）必须使用 auto，否则会拒绝强制工具调用。
# 可在「偏好设置 → 模型」中按提供商切换：自动 / 必须 / 省略。

[feature_prefs.tools.shell]
hidden = false
default_enabled = true

[feature_prefs.skills.pdf]
hidden = false
default_enabled = false

[audio.asr]
backend = "cloud"        # off | cloud | system
provider = "deepseek"
model = "whisper-compatible-model"

[audio.tts]
backend = "system"       # off | cloud | system
rate = 0
volume = 100

[media.image]
provider = "智谱"
model = "cogview-3-flash"

[[mcp_servers]]
name = "filesystem"
enabled = true
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "G:\\work"]
```

## 审核

| 模式 | 行为 |
| --- | --- |
| `manual` | 每次需要审核的调用都询问你 |
| `rules` | 授权目录内自动放行，其余问你 |
| `model` | 授权目录内自动放行，其余由审核模型结合对话上下文判断，不打断你 |
| `off` | 全部放行（高风险） |

授权目录之外不是直接拒绝，而是升级：规则模式问你，模型模式问审核模型。审核模型
在偏好设置的「审核」一节里选择，可以跟随当前对话模型，也可以单独指定；没有可用审核
模型时会退回人工确认。

目录授权属于**当前会话**，随会话保存；默认没有授权，因此文件与 Shell 操作都需要审核。
授权通过底部栏的「目录」按钮管理，审核模式通过相邻的按钮切换，不需要打开设置。

## 安全说明

- `off` 模式与 MCP 的未知工具风险最高，只在你信任任务内容时使用。
- stdio MCP 服务器是以当前用户身份运行的本地程序，添加即表示信任其命令。
- 配置文件与会话文件都是本机明文，包含密钥与工具输出。

## 快捷键

- 默认 `Enter` 发送、`Shift+Enter` 换行；`Ctrl+Enter`（macOS `⌘+Enter`）始终可以发送。
- 在「设置 → 通用 → 按键」切换为 `Ctrl+Enter` 发送后，`Enter` 恢复为换行。
- 按住说话：在偏好设置里点「录制」，按下想要的组合键即可填入；默认 `Ctrl+V`。
