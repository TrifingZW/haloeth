# Haloeth

轻量桌面智能体：随时呼出、单窗口、完成具体任务。界面基于 GPUI，代理内核独立成 `agent` crate。

## 功能

- **对话式代理**：工具调用的「动作—观测」循环，流式输出、思考过程、实时活动时间线。
- **主界面快速设置**：左下角「⋯」直接切换工具、MCP、Skill；审核模式与目录授权各有常驻按钮，点击即可调整。
- **多会话**：自动保存、切换、重命名、删除；默认存放在系统数据目录，Windows 可自定义。
- **配色**：跟随系统 / 浅色 / 深色，在设置中切换；状态与开关用颜色区分（绿色启用、淡红停用/取消）。
- **工具**：读写文件、列目录、Shell、网页访问、当前时间；提供商搜索接口封装为 `web_search`（DeepSeek `POST /tools/search`）。
- **四档审核**：手动 / 规则自动 / 模型自动 / 无审核；目录可指定只读或读写，越界直接拒绝。
- **MCP**：基于官方 [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk)，支持 stdio 与 Streamable HTTP。
- **Skill**：Claude 风格目录包（`SKILL.md` + 可选资源），按需通过 `load_skill` 加载。

## 构建与运行

需要 Rust 1.85+（edition 2024）。

```sh
cargo run
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

[approval]
mode = "manual"          # manual | rules | model | off
allow_network = false
reviewer.use_current = true

[search]
enabled = true
max_results = 5

# 思考型提供商（如 DeepSeek）必须使用 auto，否则会拒绝强制工具调用。
# 可在「模型」对话框中按提供商切换：自动 / 必须 / 省略。

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
| `manual` | 每次需要审核的调用都询问 |
| `rules` | 授权目录内自动放行、越界拒绝、拿不准时询问 |
| `model` | 先走规则；拿不准的交给独立审核模型，不打断用户 |
| `off` | 全部放行（高风险） |

目录授权属于**当前会话**，随会话保存；默认没有授权，因此文件与 Shell 操作都需要审核。
授权通过底部栏的「目录」按钮管理，审核模式通过相邻的按钮切换，不需要打开设置。

## 安全说明

- `off` 模式与 MCP 的未知工具风险最高，只在你信任任务内容时使用。
- stdio MCP 服务器是以当前用户身份运行的本地程序，添加即表示信任其命令。
- 配置文件与会话文件都是本机明文，包含密钥与工具输出。

## 快捷键

- `Ctrl+Enter`（macOS `⌘+Enter`）提交，`Enter` 换行。
