# Haloeth 远程协议规范（v1）

面向客户端实现者的线协议规范。手机、平板、浏览器、CLI 等设备按本规范实现客户端，即可获得与桌面窗口趋近原生的完整交互。

设计动机与备选决策见 [`remote-design.md`](remote-design.md)；本文件仅定义协议行为。

## 1. 概述

- 宿主：桌面端 Haloeth 进程，是唯一的事实来源与命令执行者。
- 参考实现：本仓库提供 `remote::client`（Rust）、`examples/remote-cli.rs` 与内置 Web 客户端（`GET /`）；第三方可按本规范独立实现客户端。
- 模型：镜像模式——客户端操作直接作用于桌面当前状态；所有连接看到同一份投影。
- 同步：服务端推送细粒度语义通知 + 离散边界上的权威快照；客户端是纯 reducer；断线重连走全量重新同步。

## 2. 传输与编码

| 项       | 值                                                                           |
| -------- | ---------------------------------------------------------------------------- |
| 传输     | WebSocket（RFC 6455）；`ws://` 或 `wss://`（自签证书，客户端按配对信息中的指纹钉扎） |
| 端点     | `GET /v1`（HTTP 升级）；路径含版本，`/v2` 可并存                             |
| 编码     | 每帧一条 UTF-8 JSON 文本消息                                                 |
| 帧上限   | 1 MiB；超限连接被关闭（4413）                                                |
| 保活     | 服务端每 20 秒发协议层 ping；`ping` 操作供应用层保活与 RTT                   |
| 资源端点 | `GET /v1/assets`（图片）；`POST /v1/attachments`（附件上传）。见第 11 节 |

## 3. 连接与鉴权

```text
1. 建立 WebSocket 连接
2. 鉴权（10 秒窗口，超时关闭 4408）：
   a. 请求头 Authorization: Bearer <token>（可选）
   b. 否则首帧必须是 hello 请求，params 携带 token
3. 成功 → hello 的响应（welcome）携带初始 state 与 sessions
4. 服务端立即推送 transcript.reset（当前会话实时快照）
5. 稳态：请求串行处理，通知并行扇出
```

- 鉴权前发送任何其它帧 → 关闭 4401；令牌无效或已被撤销 → 关闭 4403。
- 同一时刻允许多个连接（多设备），通知向全部连接扇出。
- `Origin` 头存在时（浏览器环境）必须在宿主配置的允许列表内或与请求同源，否则拒绝升级（HTTP 403）。
- 令牌校验失败计数按来源地址限速；超限的请求被拒绝（HTTP 429，WebSocket 升级与 HTTP 端点均适用）。
- `wss://` 使用宿主生成的自签证书；客户端按配对信息中的 `fp`（证书 DER 的 SHA-256 指纹）钉扎，其余流程与明文完全一致。

## 4. 消息信封

三类消息共用「版本 + 操作」骨架；`v` 为协议版本（当前恒为 `1`）。

```json
// 请求：客户端 → 服务端。id 为客户端自增序号，用于关联响应。
{ "v": 1, "id": 7, "op": "message.send", "params": { "text": "你好" } }

// 成功响应：服务端 → 客户端。id 与请求配对。
{ "v": 1, "id": 7, "ok": true, "result": {} }

// 失败响应：code 见第 9 节；data 为可选结构化补充。
{ "v": 1, "id": 7, "ok": false,
  "error": { "code": "busy", "message": "运行中", "data": { "text": "你好" } } }

// 通知：服务端 → 客户端，无 id，无需应答；seq 为通知序号。
{ "v": 1, "seq": 42, "op": "message.delta", "params": { "text": "正在" } }
```

规则：

- 客户端消息全部是请求（必须携带 `id`）；服务端消息分为响应（有 `id`）与通知（无 `id`）。
- `params` 与 `result` 缺省为 `{}`；未知字段必须忽略（前向兼容）。
- 响应与通知两类消息之间不保证先后（服务端各自异步写出）；同一类消息内部保序。客户端不得依赖响应与通知的相对顺序。
- `seq` 仅出现在通知信封：宿主发布时分配，服务进程生命周期内严格递增，重启后重新计数。连接建立时合成的 `transcript.reset` 携带计数器高水位（不消耗计数器值）；会话切换时推送的 `transcript.reset` 按普通通知分配序号。
- 客户端维护游标（已见最大 `seq`）；丢弃 `seq` 不大于游标的通知，其效果已包含在快照内或已处理。游标用于 `session.resume` 增量续接（见第 5 节）。
- 服务端无法识别的操作名 → 失败响应 `unknown_op`；通知方向无法识别的 `op` 直接丢弃。

## 5. 请求目录

| op                 | params                   | result                  | 说明                                                                              |
| ------------------ | ------------------------ | ----------------------- | --------------------------------------------------------------------------------- |
| `hello`            | `{ token, client? }`     | `Welcome`               | 鉴权 + 协议版本确认；每连接仅一次                                                 |
| `ping`             | `{}`                     | `{ "t": 1730000000 }`   | 应用层保活；`t` 为服务端 Unix 毫秒                                                |
| `state.get`        | `{}`                     | `{ "state": AppState }` | 全量状态兜底刷新                                                                  |
| `message.send`    | `{ text, attachments? }`                 | `{}`                | 运行中返回 `busy`，`data.text` 回显原文；`attachments` 为附件标识（见第 11 节） |
| `run.cancel`       | `{}`                     | `{}`                    | 停止当前运行，拒绝全部挂起审批                                                    |
| `approval.resolve` | `{ id, decision }`       | `{}`                    | 幂等；重复 / 过期 id 视为成功                                                     |
| `session.new`      | `{}`                     | `{ "id": "…" }`         | 新建空白会话并切换                                                                |
| `session.open`     | `{ id }`                 | `{ "id": "…" }`         | 切换会话（桌面跟随）；运行中返回 `busy`                                           |
| `session.rename`   | `{ title }`              | `{}`                    | 重命名当前会话                                                                    |
| `session.delete`   | `{ id }`                 | `{}`                    | 删除指定会话；运行中返回 `busy`                                                   |
| `model.select`     | `{ provider, model }`    | `{}`                    | 会话级模型覆盖；未知组合返回 `unknown_model`                                      |
| `feature.set`      | `{ key, enabled }`       | `{}`                    | `tool:name` / `mcp:name` / `skill:name`                                           |
| `grant.add`        | `{ path, access }`       | `{}`                    | `access`: `read` / `write`                                                        |
| `grant.remove`     | `{ path }`               | `{}`                    |                                                                                   |
| `approval.mode`    | `{ mode }`               | `{}`                    | 全局审核模式                                                                      |
| `session.resume`   | `{ session_id, last_seq }` | `{}`                 | 增量续接：可补齐时按原序号补发 `seq` 大于 `last_seq` 的通知（先于响应写出）；不可补齐时（会话不匹配、重启、超出保留窗口）推送当前会话快照 `transcript.reset`（`seq` 为高水位） |

`grant.add` / `grant.remove` 与桌面「目录授权」等价：写入的目录进入会话授权列表，下一轮运行生效。

`Welcome`：

```json
{
  "protocol": 1,
  "server": { "name": "haloeth", "version": "0.2.0", "platform": "windows" },
  "state": {/* AppState，见第 7 节 */},
  "sessions": [/* SessionSummary[] */]
}
```

## 6. 通知目录

| op                   | params                                         | 时机                                     |
| -------------------- | ---------------------------------------------- | ---------------------------------------- |
| `transcript.reset`   | `{ session_id, entries, streaming }`           | 连接后、会话切换 / 恢复                  |
| `message.appended`   | `{ role, text }`                               | 追加完整文本行（用户消息、兜底答复等）   |
| `message.delta`      | `{ text }`                                     | 助手流式增量                             |
| `message.completed`  | `{ text }`                                     | 一个记忆步骤完成；`text` 可为 `null`     |
| `reasoning.delta`    | `{ text }`                                     | 思考过程增量（合并进当前思考卡片）       |
| `tool.started`       | `{ call_id, name, arguments }`                 | 工具调用开始                             |
| `tool.finished`      | `{ call_id, ok, preview, elapsed_ms }`         | 工具调用结束                             |
| `approval.requested` | `{ id, tool, summary, risk, manual, effects }` | 审批请求                                 |
| `approval.resolved`  | `{ id, decision, reason }`                     | 审批出结果（任意端解决，含桌面端）       |
| `images.appended`    | `{ items: [{ path, caption }] }`               | 图片产出                                 |
| `plan.appended`      | `{ text }`                                     | 计划文本                                 |
| `status.changed`     | `{ status, activity }`                         | 会话状态或活动文案变化                   |
| `usage.changed`      | `{ total, by_model }`                          | 用量累计或校准                           |
| `sessions.changed`   | `{ summaries }`                                | 会话列表变化（保存、增删改、切换）       |
| `state.changed`      | `{ state }`                                    | 模型选择、功能开关、授权、审核模式等变化 |
| `shutdown`           | `{ reason }`                                   | 服务端主动停止（如用户关闭远程）         |

## 7. 数据类型

枚举值与内部模型一一对应，均为 `snake_case` 字符串。

**会话状态**（`status`）：`idle` / `preparing` / `running` / `awaiting_approval` / `cancelling` / `unconfigured` / `unselected` / `failed`。

**活动卡片状态**：`running` / `ok` / `failed` / `pending` / `allowed` / `denied` / `interrupted`。

**角色**：`user` / `assistant`。**审批决定**：`allow_once` / `allow_always` / `deny`。
**风险**：`low` / `medium` / `high`。**访问级别**：`read` / `write`。
**审核模式**：`manual` / `rules` / `model` / `off`。

**SessionSummary**：

```json
{
  "id": "0a1b…",
  "title": "整理下载目录",
  "updated_at": 1730000000000,
  "message_count": 6
}
```

**TranscriptEntry**（`entries` 数组元素，三种互斥形态）：

```json
{ "type": "text", "role": "user", "text": "列出当前目录" }

{ "type": "activity", "id": "call_ab12", "title": "调用 list_dir",
  "detail": "{\"path\":\".\"}", "preview": "…", "elapsed_ms": 34,
  "status": "ok", "expanded": false }

{ "type": "image", "path": "C:\\out\\img.png", "caption": "生成的图片" }
```

`preview`、`elapsed_ms` 可为 `null`。风险等级仅随 `approval.requested` 实时携带；`transcript.reset` 后不再保留，与桌面持久化行为一致。

**用量**（`usage.changed` / `AppState.usage`）：

```json
{
  "total": { "input": 12000, "output": 3400 },
  "by_model": { "deepseek-chat": { "input": 12000, "output": 3400 } }
}
```

**AppState**（`state.get` / `Welcome.state` / `state.changed`）：

```json
{
  "active_session_id": "0a1b…",
  "title": "整理下载目录",
  "status": "idle",
  "activity": null,
  "selection": { "provider": "deepseek", "model": "deepseek-chat" },
  "models": [{ "provider": "deepseek", "model": "deepseek-chat" }],
  "features": [
    {
      "key": "tool:web_search",
      "kind": "tool",
      "name": "web_search",
      "enabled": true
    }
  ],
  "grants": [{ "path": "D:\\work", "access": "write" }],
  "approval_mode": "rules",
  "usage": {/* 同上 */}
}
```

`models` 已按「对话可用（文本→文本）」过滤，客户端直接渲染选择器，不接收模型目录。`selection` 可为 `null`（未选择模型）。

## 8. 客户端渲染状态机

客户端维护三个槽位：行列表（transcript）、状态槽（status / usage / sessions / state）、当前流式消息。规则按桌面 `Conversation` 的行为逐条对齐：

1. `message.delta`：无打开的流式消息则新建一条 `assistant` 行；否则追加。
2. `message.completed`：关闭当前流式消息；`text` 非空时追加一条完整 `assistant` 行。`text` 为 `null` 只表示收束，无新行。
3. `message.appended`：关闭当前流式消息，追加指定行。
4. `reasoning.delta`：合并进当前思考卡片（标题固定「思考过程」）；无则新建。思考卡片与流式消息可并存。
5. `tool.started`：关闭当前流式消息与思考卡片；新增活动卡片（`running`），`detail` 为参数摘要。
6. `tool.finished` / `approval.resolved`：按 `call_id` / `id` 更新对应卡片的状态、预览、耗时。
7. `approval.requested`：新增审批卡片；`manual` 为真时状态 `pending`（等待人工），否则 `running`（审核模型判定中）。
8. `images.appended` / `plan.appended`：追加图片行 / 计划卡片。
9. `transcript.reset`：整体替换行列表与流式消息（`streaming` 为 `{ role, text }` 或 `null`，非空时恢复为打开中的流式消息），并将 `seq` 游标重置为该通知的 `seq`（连接时的快照水位或普通事件序号）。
10. `status.changed` / `usage.changed` / `sessions.changed` / `state.changed`：覆盖对应状态槽。
11. `seq` 去重：丢弃 `seq` 不大于游标的通知；游标由更大的 `seq` 与 `transcript.reset` 推进（见第 4 节）。

断线恢复：任意原因断开后退避重连，重新执行自 `hello` 起的全量同步；需要接续时随后可发送 `session.resume`，服务端可补齐则按原序号补发，否则推送快照。

## 9. 错误码与关闭码

请求错误（`error.code`）：

| code              | 场景                                          |
| ----------------- | --------------------------------------------- |
| `unauthorized`    | 未鉴权即发请求                                |
| `busy`            | 运行中执行被拒绝的操作（`data` 携带回显文本） |
| `not_found`       | 会话等目标不存在                              |
| `unknown_model`   | 模型组合不可用                                |
| `unknown_feature` | 功能键不存在                                  |
| `unknown_op`      | 操作名无法识别                                |
| `invalid_params`   | 参数缺失或类型错误                           |
| `internal`         | 宿主内部错误（含错误描述，不泄露路径等细节） |

连接关闭码（WebSocket close code）：

| code | 含义                             |
| ---- | -------------------------------- |
| 1000 | 正常关闭（含 `shutdown` 后）     |
| 4400 | 信封解析失败（非 JSON / 缺字段） |
| 4401 | 鉴权前发送了非 hello 帧          |
| 4403 | 令牌无效或已撤销                 |
| 4408 | 鉴权超时                         |
| 4409 | 协议版本不匹配                   |
| 4413 | 单帧超过 1 MiB                   |
| 4429 | 通知积压（慢客户端），请重连同步 |
| 4500 | 服务端内部错误                   |

## 10. 兼容性规则

- 协议版本逐帧标记：每条消息的 `v` 是发送方的大版本号；接收方不支持时以 4409 关闭。校验在 `hello` 阶段完成，避免连接中途失效。
- 新增字段：客户端必须忽略未知字段；服务端序列化保持「稳定字段名 + `snake_case`」。
- 新增操作：旧客户端收到未知 `op` 的失败响应（请求）或直接丢弃（通知）。
- 枚举只增不改：废弃值保留序列化兼容。
- 线格式的任何破坏性变更必须升 `/v2` 端点与协议版本号，`/v1` 保持可用。

## 11. 资源与附件端点

通知里的图片是宿主机本地路径；客户端按需拉取：

```text
GET /v1/assets?path=<URL-encoded 绝对路径>&token=<设备令牌>
→ 200 image/*（字节流，按内容探测 MIME）
→ 404 未被公布的路径
```

- 仅允许拉取经 `images.appended` 公布过的路径（宿主维护有界白名单，容量 256，先进先出）。
- 令牌置于查询串是为支持 `<img src>` 场景；设备令牌可撤销，泄露的影响限于该设备。

附件上传：

```text
POST /v1/attachments?token=<设备令牌>（或 Authorization: Bearer 头）
Content-Type: multipart/form-data；字段 file 为文件内容
→ 200 { "attachment_id": "…", "mime": "…", "size": 12345 }
→ 400 缺少 file 字段或文件为空
→ 401 令牌无效或已撤销
→ 413 单文件超过 20 MiB
→ 415 类型不被接受（MIME 按内容探测）
```

- 接受的类型：常见图片格式，以及不含 NUL 的 UTF-8 文本；其余一律 415。
- 附件归属当前会话，存储于应用数据目录 `attachments/{session_id}/`，不落入任何已授权目录；该目录随会话获得隐式只读授权（同 Skill 目录模式），写入仍走审批。
- `message.send` 的可选参数 `attachments: [attachment_id]` 由服务端解析为绝对路径，并把 `[附件] <路径>` 行前置到用户消息文本；模型据此按路径读取。未知标识返回 `invalid_params`。
- 上传是经过鉴权的用户动作，写入目标为应用数据目录，不触发目录授权审批；会话删除时清理其附件。

## 12. 示例交互流

一次完整往返（`C:` 客户端发送，`S:` 服务端发送）：

```json
C: { "v": 1, "id": 1, "op": "hello", "params": { "token": "…", "client": { "name": "web", "version": "1.0" } } }
S: { "v": 1, "id": 1, "ok": true, "result": { "protocol": 1, "server": { "name": "haloeth", "version": "0.2.0", "platform": "windows" },
     "state": { "active_session_id": "s1", "status": "idle", "…": "…" }, "sessions": [ { "id": "s1", "title": "整理下载目录", "updated_at": 1730000000000, "message_count": 0 } ] } }
S: { "v": 1, "seq": 0, "op": "transcript.reset", "params": { "session_id": "s1", "entries": [], "streaming": null } }

C: { "v": 1, "id": 2, "op": "message.send", "params": { "text": "整理下载目录里的截图" } }
S: { "v": 1, "id": 2, "ok": true, "result": {} }
S: { "v": 1, "seq": 1, "op": "message.appended", "params": { "role": "user", "text": "整理下载目录里的截图" } }
S: { "v": 1, "seq": 2, "op": "status.changed", "params": { "status": "preparing", "activity": "连接中…" } }
S: { "v": 1, "seq": 3, "op": "status.changed", "params": { "status": "running", "activity": "思考中…" } }
S: { "v": 1, "seq": 4, "op": "reasoning.delta", "params": { "text": "先列出目录内容…" } }
S: { "v": 1, "seq": 5, "op": "tool.started", "params": { "call_id": "call_1", "name": "list_dir", "arguments": "{\"path\":\"…\"}" } }
S: { "v": 1, "seq": 6, "op": "status.changed", "params": { "status": "running", "activity": "调用 list_dir 中…" } }
S: { "v": 1, "seq": 7, "op": "tool.finished", "params": { "call_id": "call_1", "ok": true, "preview": "…", "elapsed_ms": 41 } }
S: { "v": 1, "seq": 8, "op": "approval.requested", "params": { "id": "ap_1", "tool": "write_file", "summary": "写入 sort.ps1", "risk": "high", "manual": true, "effects": ["write"] } }
S: { "v": 1, "seq": 9, "op": "status.changed", "params": { "status": "awaiting_approval", "activity": "整理结果…" } }

C: { "v": 1, "id": 3, "op": "approval.resolve", "params": { "id": "ap_1", "decision": "allow_once" } }
S: { "v": 1, "id": 3, "ok": true, "result": {} }
S: { "v": 1, "seq": 10, "op": "approval.resolved", "params": { "id": "ap_1", "decision": "allow_once", "reason": "" } }
S: { "v": 1, "seq": 11, "op": "status.changed", "params": { "status": "running", "activity": "继续运行…" } }
S: { "v": 1, "seq": 12, "op": "message.delta", "params": { "text": "已整理完成，" } }
S: { "v": 1, "seq": 13, "op": "message.delta", "params": { "text": "共移动 12 张截图。" } }
S: { "v": 1, "seq": 14, "op": "message.completed", "params": { "text": null } }
S: { "v": 1, "seq": 15, "op": "status.changed", "params": { "status": "idle", "activity": null } }
S: { "v": 1, "seq": 16, "op": "usage.changed", "params": { "total": { "input": 8100, "output": 900 }, "by_model": { "deepseek-chat": { "input": 8100, "output": 900 } } } }
S: { "v": 1, "seq": 17, "op": "sessions.changed", "params": { "summaries": [ { "id": "s1", "title": "整理下载目录里的截图", "updated_at": 1730000060000, "message_count": 2 } ] } }
```

示例中 `message.completed` 的 `text` 为 `null`：最终答复已通过 `message.delta` 流式送达，步骤本身无新增文本，与桌面「流式回复不在步骤完成时重复渲染」的行为一致。

示例中 `transcript.reset` 的 `seq` 为 0（连接时尚无已发布通知时的水位），后续通知自 1 起严格递增，客户端游标随之推进。
