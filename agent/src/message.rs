//! 对话消息的内部表示、流式增量聚合与提供方协议投影。

use crate::metrics::TokenUsage;

use serde_json::{Map, Value};

use std::collections::BTreeMap;

/// 内部对话角色。
///
/// 前三个角色与提供方协议一致；后两个角色用于区分「工具调用指令」与「工具执行结果」，
/// 投影为提供方协议时分别并入助手消息与工具消息。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Role {
    /// 系统指令。
    System,
    /// 用户输入。
    User,
    /// 模型输出。
    Assistant,
    /// 模型发起的工具调用集合，属于助手回合的组成部分。
    ToolCall,
    /// 工具执行结果。
    ToolResponse,
}

/// 消息内容块。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ContentBlock {
    /// 文本块。
    Text(String),
    /// 图像块。
    Image(ImageSource),
}

impl ContentBlock {
    /// 以文本构造内容块。
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    /// 以资源地址构造图像块。
    pub fn image_url(url: impl Into<String>) -> Self {
        Self::Image(ImageSource::Url(url.into()))
    }

    /// 以 `Base64` 数据构造图像块。
    pub fn image_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Image(ImageSource::Base64 {
            media_type: media_type.into(),
            data: data.into(),
        })
    }
}

/// 图像数据来源。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ImageSource {
    /// 资源地址。
    Url(String),
    /// `Base64` 编码的内联数据。
    Base64 {
        /// 媒体类型。
        media_type: String,
        /// 编码后的数据。
        data: String,
    },
}

impl ImageSource {
    /// 返回提供方协议的图像地址；内联数据编码为数据地址。
    pub fn to_url(&self) -> String {
        match self {
            Self::Url(url) => url.clone(),
            Self::Base64 {
                media_type,
                data,
            } => format!("data:{media_type};base64,{data}"),
        }
    }
}

/// 一次工具调用。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    /// 调用标识符，用于关联工具执行结果。
    pub id: String,
    /// 被调用工具的名称。
    pub name: String,
    /// 已解析的调用参数；无参数以空对象表示。
    pub arguments: Value,
}

/// 一条对话消息。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Message {
    /// 内部角色。
    pub role: Role,
    /// 内容块序列；空序列表示无内容。
    pub content: Vec<ContentBlock>,
    /// 工具调用集合，仅 [`Role::ToolCall`] 使用。
    pub tool_calls: Vec<ToolCall>,
    /// 所回应的调用标识符，仅 [`Role::ToolResponse`] 使用。
    pub tool_call_id: Option<String>,
    /// 本次生成消耗的 **Token** 用量。
    pub usage: Option<TokenUsage>,
}

impl Message {
    /// 以角色与内容块序列构造消息。
    pub fn new(role: Role, content: impl IntoIterator<Item = ContentBlock>) -> Self {
        Self {
            role,
            content: content.into_iter().collect(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            usage: None,
        }
    }

    /// 以文本构造指定角色的消息。
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self::new(role, [ContentBlock::text(text)])
    }

    /// 构造系统消息。
    pub fn system(text: impl Into<String>) -> Self {
        Self::text(Role::System, text)
    }

    /// 构造用户消息。
    pub fn user(text: impl Into<String>) -> Self {
        Self::text(Role::User, text)
    }

    /// 构造助手消息。
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::text(Role::Assistant, text)
    }

    /// 构造携带工具调用的消息。
    pub fn tool_calls(calls: impl IntoIterator<Item = ToolCall>) -> Self {
        Self {
            role: Role::ToolCall,
            content: Vec::new(),
            tool_calls: calls.into_iter().collect(),
            tool_call_id: None,
            usage: None,
        }
    }

    /// 构造工具执行结果消息。
    pub fn tool_response(tool_call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: Role::ToolResponse,
            content: vec![ContentBlock::text(text)],
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            usage: None,
        }
    }

    /// 附加 **Token** 用量。
    pub fn with_usage(mut self, usage: TokenUsage) -> Self {
        self.usage = Some(usage);
        self
    }
}

/// 流式响应的单个增量。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamDelta {
    /// 文本增量。
    pub content: Option<String>,
    /// 推理过程增量（提供方的 `reasoning_content` 等字段），不进入上下文。
    pub reasoning: Option<String>,
    /// 工具调用分片。
    pub tool_calls: Vec<ToolCallDelta>,
    /// 本帧携带的 **Token** 用量。
    pub usage: Option<TokenUsage>,
}

/// 工具调用的流式分片；同一调用以索引关联。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolCallDelta {
    /// 调用在本次响应中的序号。
    pub index: usize,
    /// 调用标识符，通常仅首帧携带。
    pub id: Option<String>,
    /// 工具名称，通常仅首帧携带。
    pub name: Option<String>,
    /// 参数的 **JSON** 文本分片。
    pub arguments: Option<String>,
}

/// 将增量序列聚合为一条完整的助手消息。
///
/// 聚合规则：
/// 1) 文本增量按出现顺序拼接。
/// 2) 工具调用分片按索引合并：标识符与名称取首个非空分片，参数按分片顺序拼接后解析；无法解析时保留原始文本。
/// 3) 用量取最后一个非空值，提供方通常在末帧给出累计用量。
///
/// 若分片缺失标识符或名称，聚合结果以空字符串表示，由调用方在工具调度阶段校验。
pub fn assemble(deltas: impl IntoIterator<Item = StreamDelta>) -> Message {
    let mut text = String::new();
    let mut fragments: BTreeMap<usize, ToolCallFragment> = BTreeMap::new();
    let mut usage = None;

    for delta in deltas {
        if let Some(fragment) = delta.content {
            text.push_str(&fragment);
        }
        for call in delta.tool_calls {
            fragments.entry(call.index).or_default().merge(call);
        }
        if delta.usage.is_some() {
            usage = delta.usage;
        }
    }

    let content = if text.is_empty() {
        Vec::new()
    } else {
        vec![ContentBlock::text(text)]
    };
    let tool_calls = fragments.into_values().map(ToolCallFragment::finish).collect();

    Message {
        role: Role::Assistant,
        content,
        tool_calls,
        tool_call_id: None,
        usage,
    }
}

/// 单个工具调用的分片累加器。
#[derive(Default)]
struct ToolCallFragment {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl ToolCallFragment {
    /// 合并一个分片。
    fn merge(&mut self, delta: ToolCallDelta) {
        if let Some(id) = delta.id {
            self.id = Some(id);
        }
        if let Some(name) = delta.name {
            self.name = Some(name);
        }
        if let Some(arguments) = delta.arguments {
            self.arguments.push_str(&arguments);
        }
    }

    /// 产出聚合后的工具调用。
    fn finish(self) -> ToolCall {
        ToolCall {
            id: self.id.unwrap_or_default(),
            name: self.name.unwrap_or_default(),
            arguments: parse_arguments(&self.arguments),
        }
    }
}

/// 解析工具参数文本；空文本视为无参数，解析失败时保留原始文本。
fn parse_arguments(text: &str) -> Value {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Value::Object(Map::new());
    }
    serde_json::from_str(trimmed).unwrap_or_else(|_| Value::String(text.to_owned()))
}

/// 提供方协议投影（**OpenAI** 兼容形态）。
pub mod wire {
    use serde::Serialize;

    /// 提供方协议角色。
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "lowercase")]
    pub enum WireRole {
        System,
        User,
        Assistant,
        Tool,
    }

    /// 一条提供方协议消息。
    #[derive(Debug, Clone, PartialEq, Serialize)]
    pub struct WireMessage {
        pub role: WireRole,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub content: Option<WireContent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_calls: Option<Vec<WireToolCall>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_call_id: Option<String>,
    }

    /// 消息内容：纯文本或内容块数组。
    #[derive(Debug, Clone, PartialEq, Serialize)]
    #[serde(untagged)]
    pub enum WireContent {
        Text(String),
        Blocks(Vec<WireContentBlock>),
    }

    /// 内容块。
    #[derive(Debug, Clone, PartialEq, Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum WireContentBlock {
        Text {
            text: String,
        },
        ImageUrl {
            image_url: WireImageUrl,
        },
    }

    /// 图像地址载荷。
    #[derive(Debug, Clone, PartialEq, Serialize)]
    pub struct WireImageUrl {
        pub url: String,
    }

    /// 工具调用。
    #[derive(Debug, Clone, PartialEq, Serialize)]
    pub struct WireToolCall {
        pub id: String,
        #[serde(rename = "type")]
        pub kind: WireToolCallKind,
        pub function: WireToolFunction,
    }

    /// 工具调用类型。
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "lowercase")]
    pub enum WireToolCallKind {
        Function,
    }

    /// 工具调用的函数载荷。
    #[derive(Debug, Clone, PartialEq, Serialize)]
    pub struct WireToolFunction {
        pub name: String,
        /// 参数的 **JSON** 文本。
        pub arguments: String,
    }
}

use wire::{
    WireContent, WireContentBlock, WireImageUrl, WireMessage, WireRole, WireToolCall,
    WireToolCallKind, WireToolFunction,
};

/// 将内部消息序列投影为提供方协议消息序列。
///
/// 投影规则：
/// 1) 角色映射：系统、用户、助手角色保持不变；工具调用消息并入助手消息，工具结果消息映射为工具角色并携带调用标识符。
/// 2) 消息合并：相邻且角色相同、且不属于工具结果的消息合并；工具结果消息因携带独立标识符而不参与合并。
/// 3) 内容投影：仅含文本时投影为字符串，含图像时投影为内容块数组。
pub fn normalize(messages: &[Message]) -> Vec<WireMessage> {
    let mut output: Vec<WireMessage> = Vec::with_capacity(messages.len());
    for message in messages {
        let projected = project(message);
        match output.last_mut() {
            Some(last) if mergeable(last, &projected) => merge(last, projected),
            _ => output.push(projected),
        }
    }
    output
}

/// 将单条消息投影为提供方协议消息。
fn project(message: &Message) -> WireMessage {
    let (role, tool_call_id) = match message.role {
        Role::System => (WireRole::System, None),
        Role::User => (WireRole::User, None),
        Role::Assistant | Role::ToolCall => (WireRole::Assistant, None),
        Role::ToolResponse => (WireRole::Tool, message.tool_call_id.clone()),
    };
    WireMessage {
        role,
        content: project_content(&message.content),
        tool_calls: (!message.tool_calls.is_empty())
            .then(|| message.tool_calls.iter().map(project_call).collect()),
        tool_call_id,
    }
}

/// 投影内容块序列；空内容返回 [`None`]。
fn project_content(content: &[ContentBlock]) -> Option<WireContent> {
    if content.is_empty() {
        return None;
    }
    if let [ContentBlock::Text(text)] = content {
        return Some(WireContent::Text(text.clone()));
    }
    Some(WireContent::Blocks(content.iter().map(project_block).collect()))
}

/// 投影单个内容块。
fn project_block(block: &ContentBlock) -> WireContentBlock {
    match block {
        ContentBlock::Text(text) => WireContentBlock::Text {
            text: text.clone(),
        },
        ContentBlock::Image(source) => WireContentBlock::ImageUrl {
            image_url: WireImageUrl {
                url: source.to_url(),
            },
        },
    }
}

/// 投影单次工具调用。
fn project_call(call: &ToolCall) -> WireToolCall {
    WireToolCall {
        id: call.id.clone(),
        kind: WireToolCallKind::Function,
        function: WireToolFunction {
            name: call.name.clone(),
            arguments: call.arguments.to_string(),
        },
    }
}

/// 判断两条提供方消息是否可合并：角色相同且均不属于工具结果。
fn mergeable(last: &WireMessage, next: &WireMessage) -> bool {
    last.role == next.role && last.tool_call_id.is_none() && next.tool_call_id.is_none()
}

/// 合并两条提供方消息：内容块拼接，工具调用追加。
fn merge(last: &mut WireMessage, next: WireMessage) {
    last.content = match (last.content.take(), next.content) {
        (None, content) => content,
        (Some(content), None) => Some(content),
        (Some(WireContent::Text(left)), Some(WireContent::Text(right))) => {
            Some(WireContent::Text(format!("{left}\n{right}")))
        }
        (Some(left), Some(right)) => {
            let mut blocks = into_blocks(left);
            blocks.extend(into_blocks(right));
            Some(WireContent::Blocks(blocks))
        }
    };
    match (last.tool_calls.as_mut(), next.tool_calls) {
        (Some(existing), Some(more)) => existing.extend(more),
        (None, more) => last.tool_calls = more,
        (Some(_), None) => {}
    }
}

/// 将内容展开为内容块数组。
fn into_blocks(content: WireContent) -> Vec<WireContentBlock> {
    match content {
        WireContent::Text(text) => vec![WireContentBlock::Text {
            text,
        }],
        WireContent::Blocks(blocks) => blocks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn search_call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: "search".to_owned(),
            arguments: json!({ "query": "rust" }),
        }
    }

    #[test]
    fn projects_internal_roles_to_provider_roles() {
        let messages = [
            Message::system("system"),
            Message::user("hello"),
            Message::tool_calls([search_call("call_1")]),
            Message::tool_response("call_1", "result"),
        ];

        let wire = normalize(&messages);

        let roles: Vec<WireRole> = wire.iter().map(|message| message.role).collect();
        assert_eq!(roles, [WireRole::System, WireRole::User, WireRole::Assistant, WireRole::Tool]);
        assert_eq!(wire[3].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn merges_consecutive_messages_with_the_same_role() {
        let wire = normalize(&[Message::user("first"), Message::user("second")]);

        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0].content, Some(WireContent::Text("first\nsecond".into())));
    }

    #[test]
    fn merges_tool_calls_into_the_preceding_assistant_message() {
        let wire = normalize(&[
            Message::assistant("I will search."),
            Message::tool_calls([search_call("call_1")]),
        ]);

        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0].role, WireRole::Assistant);
        assert_eq!(wire[0].content, Some(WireContent::Text("I will search.".into())));
        let calls = wire[0].tool_calls.as_ref().expect("tool calls should be projected");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].kind, WireToolCallKind::Function);
        assert_eq!(calls[0].function.name, "search");
        assert_eq!(calls[0].function.arguments, r#"{"query":"rust"}"#);
    }

    #[test]
    fn keeps_tool_responses_separate() {
        let wire = normalize(&[
            Message::tool_response("call_1", "a"),
            Message::tool_response("call_2", "b"),
        ]);

        assert_eq!(wire.len(), 2);
        assert_eq!(wire[0].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(wire[1].tool_call_id.as_deref(), Some("call_2"));
        assert_eq!(wire[1].content, Some(WireContent::Text("b".into())));
    }

    #[test]
    fn projects_images_as_content_blocks() {
        let message = Message::new(
            Role::User,
            [
                ContentBlock::text("look"),
                ContentBlock::image_base64("image/png", "AAAA"),
                ContentBlock::image_url("https://example.com/a.png"),
            ],
        );

        let wire = normalize(&[message]);

        let expected = WireContent::Blocks(vec![
            WireContentBlock::Text {
                text: "look".into(),
            },
            WireContentBlock::ImageUrl {
                image_url: WireImageUrl {
                    url: "data:image/png;base64,AAAA".into(),
                },
            },
            WireContentBlock::ImageUrl {
                image_url: WireImageUrl {
                    url: "https://example.com/a.png".into(),
                },
            },
        ]);
        assert_eq!(wire[0].content, Some(expected));
    }

    #[test]
    fn omits_content_and_tool_calls_when_absent() {
        let wire = normalize(&[Message::tool_calls([])]);

        assert_eq!(wire[0].content, None);
        assert_eq!(wire[0].tool_calls, None);
    }

    #[test]
    fn serializes_to_provider_protocol_shape() {
        let wire = normalize(&[
            Message::user("hello"),
            Message::tool_calls([search_call("call_1")]),
            Message::tool_response("call_1", "ok"),
        ]);

        let json = serde_json::to_value(&wire).expect("serialization should succeed");

        assert_eq!(json[0], json!({ "role": "user", "content": "hello" }));
        assert_eq!(
            json[1],
            json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "search", "arguments": "{\"query\":\"rust\"}" },
                }],
            })
        );
        assert_eq!(json[2], json!({ "role": "tool", "tool_call_id": "call_1", "content": "ok" }));
    }

    #[test]
    fn assembles_content_fragments() {
        let deltas = [
            StreamDelta {
                content: Some("Hel".into()),
                ..Default::default()
            },
            StreamDelta {
                content: Some("lo".into()),
                ..Default::default()
            },
        ];

        let message = assemble(deltas);

        assert_eq!(message.role, Role::Assistant);
        assert_eq!(message.content, vec![ContentBlock::text("Hello")]);
        assert!(message.tool_calls.is_empty());
    }

    #[test]
    fn assembles_tool_call_fragments_by_index() {
        let deltas = [
            StreamDelta {
                tool_calls: vec![ToolCallDelta {
                    index: 1,
                    id: Some("call_b".into()),
                    name: Some("second".into()),
                    arguments: Some(r#"{"a":"#.into()),
                }],
                ..Default::default()
            },
            StreamDelta {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_a".into()),
                    name: Some("first".into()),
                    arguments: Some("{}".into()),
                }],
                ..Default::default()
            },
            StreamDelta {
                tool_calls: vec![ToolCallDelta {
                    index: 1,
                    arguments: Some("1}".into()),
                    ..Default::default()
                }],
                ..Default::default()
            },
        ];

        let message = assemble(deltas);

        assert_eq!(message.tool_calls.len(), 2);
        assert_eq!(message.tool_calls[0].name, "first");
        assert_eq!(message.tool_calls[1].id, "call_b");
        assert_eq!(message.tool_calls[1].arguments, json!({ "a": 1 }));
    }

    #[test]
    fn keeps_unparseable_arguments_as_text() {
        let deltas = [StreamDelta {
            tool_calls: vec![ToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                name: Some("run".into()),
                arguments: Some("not json".into()),
            }],
            ..Default::default()
        }];

        assert_eq!(assemble(deltas).tool_calls[0].arguments, Value::String("not json".into()));
    }

    #[test]
    fn treats_missing_arguments_as_empty_object() {
        let deltas = [StreamDelta {
            tool_calls: vec![ToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                name: Some("run".into()),
                arguments: None,
            }],
            ..Default::default()
        }];

        assert_eq!(assemble(deltas).tool_calls[0].arguments, json!({}));
    }

    #[test]
    fn takes_the_last_usage_frame() {
        let deltas = [
            StreamDelta {
                usage: Some(TokenUsage::new(10, 1)),
                ..Default::default()
            },
            StreamDelta {
                usage: Some(TokenUsage::new(10, 7)),
                ..Default::default()
            },
        ];

        assert_eq!(assemble(deltas).usage, Some(TokenUsage::new(10, 7)));
    }
}
