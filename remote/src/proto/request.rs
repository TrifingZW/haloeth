//! 请求信封与命令解析。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::dto::{AccessDto, ApprovalDecisionDto, ApprovalModeDto};
use super::response::RemoteError;

/// 请求信封：客户端 → 服务端。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestFrame {
    /// 协议版本。
    pub v: u8,
    /// 客户端自增序号，用于关联响应。
    pub id: u64,
    /// 操作名。
    pub op: String,
    /// 操作参数。
    #[serde(default)]
    pub params: Value,
}

/// 握手参数；`token` 在携带 `Authorization` 头时可省略。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HelloParams {
    /// 设备令牌。
    #[serde(default)]
    pub token: Option<String>,
    /// 客户端自述信息。
    #[serde(default)]
    pub client: Option<ClientInfo>,
}

/// 客户端自述信息，仅用于日志。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// 名称。
    #[serde(default)]
    pub name: String,
    /// 版本。
    #[serde(default)]
    pub version: String,
}

/// 已解析的强类型命令。
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// 服务端内部：组装握手响应体，对应协议操作 `hello`。
    Welcome,
    /// 服务端内部：连接后的实时快照与通知水位。
    Snapshot,
    /// 应用层保活与 RTT。
    Ping,
    /// 全量状态刷新。
    StateGet,
    /// 服务端内部：附件上传的目标目录与隐式授权。
    AttachmentTarget,
    /// 发送一条用户消息。
    MessageSend {
        /// 消息文本。
        text: String,
        /// 附件：协议层为附件标识，服务端解析后为绝对路径。
        attachments: Vec<String>,
    },
    /// 取消当前运行。
    RunCancel,
    /// 解决一次审批。
    ApprovalResolve {
        /// 审批请求标识。
        id: String,
        /// 决定。
        decision: ApprovalDecisionDto,
    },
    /// 新建空白会话并切换。
    SessionNew,
    /// 切换会话。
    SessionOpen {
        /// 会话标识符。
        id: String,
    },
    /// 重命名当前会话。
    SessionRename {
        /// 新标题。
        title: String,
    },
    /// 删除会话。
    SessionDelete {
        /// 会话标识符。
        id: String,
    },
    /// 选择会话级模型。
    ModelSelect {
        /// 提供商名称。
        provider: String,
        /// 模型标识。
        model: String,
    },
    /// 设置功能开关。
    FeatureSet {
        /// `tool:name` / `mcp:name` / `skill:name`。
        key: String,
        /// 是否启用。
        enabled: bool,
    },
    /// 添加目录授权。
    GrantAdd {
        /// 目录路径。
        path: String,
        /// 访问级别。
        access: AccessDto,
    },
    /// 移除目录授权。
    GrantRemove {
        /// 目录路径。
        path: String,
    },
    /// 设置全局审核模式。
    ApprovalMode {
        /// 模式。
        mode: ApprovalModeDto,
    },
}

impl Command {
    /// 解析请求信封；未知操作与非法参数返回结构化错误。
    pub fn parse(frame: &RequestFrame) -> Result<Self, RemoteError> {
        let params = frame.params.clone();
        match frame.op.as_str() {
            "ping" => Ok(Self::Ping),
            "state.get" => Ok(Self::StateGet),
            "message.send" => {
                #[derive(Deserialize)]
                struct Params {
                    text: String,
                    #[serde(default)]
                    attachments: Vec<String>,
                }
                let parsed: Params = parse_params(params)?;
                let text = parsed.text.trim().to_owned();
                if text.is_empty() {
                    return Err(RemoteError::invalid_params("text 不能为空"));
                }
                Ok(Self::MessageSend {
                    text,
                    attachments: parsed.attachments,
                })
            }
            "run.cancel" => Ok(Self::RunCancel),
            "approval.resolve" => {
                #[derive(Deserialize)]
                struct Params {
                    id: String,
                    decision: ApprovalDecisionDto,
                }
                let parsed: Params = parse_params(params)?;
                check_id(&parsed.id)?;
                Ok(Self::ApprovalResolve {
                    id: parsed.id,
                    decision: parsed.decision,
                })
            }
            "session.new" => Ok(Self::SessionNew),
            "session.open" => {
                #[derive(Deserialize)]
                struct Params {
                    id: String,
                }
                let parsed: Params = parse_params(params)?;
                check_id(&parsed.id)?;
                Ok(Self::SessionOpen {
                    id: parsed.id,
                })
            }
            "session.rename" => {
                #[derive(Deserialize)]
                struct Params {
                    title: String,
                }
                let parsed: Params = parse_params(params)?;
                let title = parsed.title.trim().to_owned();
                if title.is_empty() {
                    return Err(RemoteError::invalid_params("title 不能为空"));
                }
                Ok(Self::SessionRename {
                    title,
                })
            }
            "session.delete" => {
                #[derive(Deserialize)]
                struct Params {
                    id: String,
                }
                let parsed: Params = parse_params(params)?;
                check_id(&parsed.id)?;
                Ok(Self::SessionDelete {
                    id: parsed.id,
                })
            }
            "model.select" => {
                #[derive(Deserialize)]
                struct Params {
                    provider: String,
                    model: String,
                }
                let parsed: Params = parse_params(params)?;
                let provider = parsed.provider.trim().to_owned();
                let model = parsed.model.trim().to_owned();
                if provider.is_empty() || model.is_empty() {
                    return Err(RemoteError::invalid_params("provider 与 model 不能为空"));
                }
                Ok(Self::ModelSelect {
                    provider,
                    model,
                })
            }
            "feature.set" => {
                #[derive(Deserialize)]
                struct Params {
                    key: String,
                    enabled: bool,
                }
                let parsed: Params = parse_params(params)?;
                if parsed.key.trim().is_empty() {
                    return Err(RemoteError::invalid_params("key 不能为空"));
                }
                Ok(Self::FeatureSet {
                    key: parsed.key.trim().to_owned(),
                    enabled: parsed.enabled,
                })
            }
            "grant.add" => {
                #[derive(Deserialize)]
                struct Params {
                    path: String,
                    access: AccessDto,
                }
                let parsed: Params = parse_params(params)?;
                if parsed.path.trim().is_empty() {
                    return Err(RemoteError::invalid_params("path 不能为空"));
                }
                Ok(Self::GrantAdd {
                    path: parsed.path.trim().to_owned(),
                    access: parsed.access,
                })
            }
            "grant.remove" => {
                #[derive(Deserialize)]
                struct Params {
                    path: String,
                }
                let parsed: Params = parse_params(params)?;
                if parsed.path.trim().is_empty() {
                    return Err(RemoteError::invalid_params("path 不能为空"));
                }
                Ok(Self::GrantRemove {
                    path: parsed.path.trim().to_owned(),
                })
            }
            "approval.mode" => {
                #[derive(Deserialize)]
                struct Params {
                    mode: ApprovalModeDto,
                }
                let parsed: Params = parse_params(params)?;
                Ok(Self::ApprovalMode {
                    mode: parsed.mode,
                })
            }
            other => Err(RemoteError::unknown_op(other)),
        }
    }

    /// 操作名；供测试与日志使用。
    pub fn op(&self) -> &'static str {
        match self {
            Self::Welcome => "hello",
            Self::Snapshot => "snapshot",
            Self::Ping => "ping",
            Self::StateGet => "state.get",
            Self::AttachmentTarget => "attachment.target",
            Self::MessageSend {
                ..
            } => "message.send",
            Self::RunCancel => "run.cancel",
            Self::ApprovalResolve {
                ..
            } => "approval.resolve",
            Self::SessionNew => "session.new",
            Self::SessionOpen {
                ..
            } => "session.open",
            Self::SessionRename {
                ..
            } => "session.rename",
            Self::SessionDelete {
                ..
            } => "session.delete",
            Self::ModelSelect {
                ..
            } => "model.select",
            Self::FeatureSet {
                ..
            } => "feature.set",
            Self::GrantAdd {
                ..
            } => "grant.add",
            Self::GrantRemove {
                ..
            } => "grant.remove",
            Self::ApprovalMode {
                ..
            } => "approval.mode",
        }
    }
}

/// 反序列化参数；失败即非法参数。
fn parse_params<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, RemoteError> {
    serde_json::from_value(params).map_err(|error| RemoteError::invalid_params(error.to_string()))
}

/// 拒绝空标识符。
fn check_id(id: &str) -> Result<(), RemoteError> {
    if id.trim().is_empty() {
        return Err(RemoteError::invalid_params("id 不能为空"));
    }
    Ok(())
}
