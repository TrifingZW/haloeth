//! 工具契约：声明式元数据、调用前参数校验、注册表与协议投影。
//!
//! 与 smolagents 的差异有两点，均为动作空间选择的结果：
//! 1) 不提供代码签名式渲染器，仅保留工具调用空间的单行描述渲染。
//! 2) 不提供惰性初始化钩子，需要延迟初始化的实现应自行使用 [`std::sync::OnceLock`]。
//!
//! 执行方法返回装箱 future：**Rust** 侧的**HTTP** 工具需要异步等待，而装箱保持契约可作 trait 对象使用。

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use serde_json::{Map, Value};

use crate::error::ToolError;

/// 工具输入与输出的类型集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputType {
    /// 字符串。
    String,
    /// 布尔值。
    Boolean,
    /// 整数。
    Integer,
    /// 数值。
    Number,
    /// 图像。
    Image,
    /// 音频。
    Audio,
    /// 数组。
    Array,
    /// 对象。
    Object,
    /// 任意类型。
    Any,
    /// 空值。
    Null,
}

impl InputType {
    /// 返回类型的规范名称。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Array => "array",
            Self::Object => "object",
            Self::Any => "any",
            Self::Null => "null",
        }
    }

    /// 判断实际类型是否被声明类型接受；空值由可空规则单独处理。
    fn accepts(self, actual: Self) -> bool {
        match self {
            Self::Any => true,
            Self::Image | Self::Audio => actual == Self::String,
            Self::Number => matches!(actual, Self::Number | Self::Integer),
            expected => expected == actual,
        }
    }

    /// 投影为 **JSON Schema** 类型名；图像、音频与任意类型投影为字符串。
    fn to_wire(self) -> wire::WireType {
        match self {
            Self::Boolean => wire::WireType::Boolean,
            Self::Integer => wire::WireType::Integer,
            Self::Number => wire::WireType::Number,
            Self::Array => wire::WireType::Array,
            Self::Object => wire::WireType::Object,
            Self::Null => wire::WireType::Null,
            Self::String | Self::Image | Self::Audio | Self::Any => wire::WireType::String,
        }
    }
}

impl std::fmt::Display for InputType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 单个输入的声明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSpec {
    /// 输入类型。
    pub ty: InputType,
    /// 输入描述，供模型理解参数含义。
    pub description: String,
    /// 是否可空：允许缺失或显式为空值。
    pub nullable: bool,
}

impl InputSpec {
    /// 构造输入声明。
    pub fn new(ty: InputType, description: impl Into<String>) -> Self {
        Self {
            ty,
            description: description.into(),
            nullable: false,
        }
    }

    /// 标记为可空。
    pub fn nullable(mut self) -> Self {
        self.nullable = true;
        self
    }
}

/// 工具元数据契约。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    /// 工具名称，须符合 [`is_valid_name`] 的规则且在注册表内唯一。
    pub name: String,
    /// 工具描述，供模型判断何时调用。
    pub description: String,
    /// 输入声明，按键名排序以保证投影与提示词的确定性。
    pub inputs: BTreeMap<String, InputSpec>,
    /// 输出类型。
    pub output_type: InputType,
    /// 输出的 **JSON Schema**，仅用于向模型声明结构化返回。
    pub output_schema: Option<Value>,
}

impl ToolSpec {
    /// 构造工具元数据。
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        output_type: InputType,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            inputs: BTreeMap::new(),
            output_type,
            output_schema: None,
        }
    }

    /// 声明一个输入。
    pub fn input(mut self, name: impl Into<String>, spec: InputSpec) -> Self {
        self.inputs.insert(name.into(), spec);
        self
    }

    /// 声明输出的 **JSON Schema**。
    pub fn output_schema(mut self, schema: Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    /// 校验元数据合法性。
    pub fn validate(&self) -> Result<(), ToolError> {
        let invalid = |reason: String| ToolError::InvalidSpec {
            name: self.name.clone(),
            reason,
        };

        if !is_valid_name(&self.name) {
            return Err(invalid("name must be an identifier of at most 64 characters".into()));
        }
        if self.description.trim().is_empty() {
            return Err(invalid("description must not be empty".into()));
        }
        for (name, input) in &self.inputs {
            if !is_valid_name(name) {
                return Err(invalid(format!(
                    "input name '{name}' must be an identifier of at most 64 characters"
                )));
            }
            if input.description.trim().is_empty() {
                return Err(invalid(format!("input '{name}' must have a description")));
            }
        }
        Ok(())
    }

    /// 渲染工具描述，供工具调用空间的系统提示使用。
    pub fn describe(&self) -> String {
        let mut description = format!("{}: {}", self.name, self.description);
        if !self.inputs.is_empty() {
            description.push_str("\n    Takes inputs:");
            for (name, input) in &self.inputs {
                let optional = if input.nullable {
                    " [optional]"
                } else {
                    ""
                };
                description.push_str(&format!(
                    "\n    - {name} ({}): {}{optional}",
                    input.ty, input.description
                ));
            }
        }
        description.push_str(&format!("\n    Returns an output of type: {}", self.output_type));
        description
    }
}

/// 工具执行返回的 future。
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send + 'a>>;

/// 工具契约。
///
/// 要求 [`Send`] 与 [`Sync`]：工具存放于注册表并跨线程共享。
pub trait Tool: Send + Sync {
    /// 返回工具元数据。
    fn spec(&self) -> &ToolSpec;

    /// 执行工具；参数已由 [`resolve_arguments`] 校验与规范化。
    fn invoke<'a>(&'a self, arguments: &'a Map<String, Value>) -> ToolFuture<'a>;
}

/// 由同步函数实现的工具。
pub struct FunctionTool<F> {
    spec: ToolSpec,
    handler: F,
}

impl<F> FunctionTool<F> {
    /// 以元数据与处理函数构造工具。
    pub fn new(spec: ToolSpec, handler: F) -> Self {
        Self {
            spec,
            handler,
        }
    }
}

impl<F> Tool for FunctionTool<F>
where
    F: Fn(&Map<String, Value>) -> Result<Value, ToolError> + Send + Sync,
{
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke<'a>(&'a self, arguments: &'a Map<String, Value>) -> ToolFuture<'a> {
        Box::pin(async move { (self.handler)(arguments) })
    }
}

/// 工具注册表：名称唯一，提供查找、执行与协议投影。
#[derive(Default)]
pub struct ToolSet {
    tools: BTreeMap<String, Box<dyn Tool>>,
}

impl ToolSet {
    /// 构造空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册工具；元数据不合法或名称重复时报错。
    pub fn insert(&mut self, tool: impl Tool + 'static) -> Result<(), ToolError> {
        tool.spec().validate()?;
        let name = tool.spec().name.clone();
        if self.tools.contains_key(&name) {
            return Err(ToolError::DuplicateName {
                name,
            });
        }
        self.tools.insert(name, Box::new(tool));
        Ok(())
    }

    /// 按名称查找工具。
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(Box::as_ref)
    }

    /// 按名称查找工具、校验参数并执行。
    pub async fn call(&self, name: &str, arguments: &Value) -> Result<Value, ToolError> {
        let tool = self.get(name).ok_or_else(|| ToolError::UnknownTool {
            name: name.to_owned(),
        })?;
        let resolved = resolve_arguments(tool.spec(), arguments)?;
        tool.invoke(&resolved).await
    }

    /// 迭代全部工具，按键名排序。
    pub fn iter(&self) -> impl Iterator<Item = &dyn Tool> {
        self.tools.values().map(Box::as_ref)
    }

    /// 工具数量。
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// 投影为提供方协议的工具声明列表。
    pub fn schemas(&self) -> Vec<wire::WireTool> {
        self.iter().map(|tool| wire::WireTool::from_spec(tool.spec())).collect()
    }

    /// 渲染工具清单，供系统提示使用。
    pub fn describe(&self) -> String {
        self.iter().map(|tool| tool.spec().describe()).collect::<Vec<_>>().join("\n")
    }
}

/// 校验并规范化工具调用参数。
///
/// 规范化与校验规则：
/// 1) 参数为对象时按键校验；为标量且工具仅声明一个输入时，视为该输入的值。
/// 2) 未声明的参数名被拒绝。
/// 3) 缺失的参数在非可空时报错。
/// 4) 显式为空值仅允许可空参数。
/// 5) 类型不匹配时报错；整数放宽为数值；图像与音频输入接受字符串（地址或 `Base64` 数据）。
pub fn resolve_arguments(
    spec: &ToolSpec,
    arguments: &Value,
) -> Result<Map<String, Value>, ToolError> {
    let invalid = |reason: String| ToolError::InvalidArguments {
        name: spec.name.clone(),
        reason,
    };

    let provided = match arguments {
        Value::Object(map) => map.clone(),
        other => {
            let mut inputs = spec.inputs.keys();
            match (inputs.next(), inputs.next()) {
                (Some(name), None) => Map::from_iter([(name.clone(), other.clone())]),
                _ => return Err(invalid("arguments must be an object".into())),
            }
        }
    };

    for name in provided.keys() {
        if !spec.inputs.contains_key(name) {
            return Err(invalid(format!("unknown argument '{name}'")));
        }
    }

    for (name, input) in &spec.inputs {
        match provided.get(name) {
            None if input.nullable => {}
            None => return Err(invalid(format!("missing required argument '{name}'"))),
            Some(Value::Null) if input.nullable => {}
            Some(Value::Null) => {
                return Err(invalid(format!("argument '{name}' must not be null")));
            }
            Some(value) => {
                let actual = value_type(value);
                if !input.ty.accepts(actual) {
                    return Err(invalid(format!(
                        "argument '{name}' has type '{actual}' but should be '{}'",
                        input.ty
                    )));
                }
            }
        }
    }

    Ok(provided)
}

/// 工具声明在提供方协议中的投影。
pub mod wire {
    use super::ToolSpec;
    use serde::Serialize;
    use std::collections::BTreeMap;

    /// 工具声明。
    #[derive(Debug, Clone, PartialEq, Serialize)]
    pub struct WireTool {
        #[serde(rename = "type")]
        pub kind: WireToolKind,
        pub function: WireFunction,
    }

    /// 工具类型。
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "lowercase")]
    pub enum WireToolKind {
        Function,
    }

    /// 函数声明。
    #[derive(Debug, Clone, PartialEq, Serialize)]
    pub struct WireFunction {
        pub name: String,
        pub description: String,
        pub parameters: WireParameters,
    }

    /// 参数模式。
    #[derive(Debug, Clone, PartialEq, Serialize)]
    pub struct WireParameters {
        #[serde(rename = "type")]
        pub kind: WireObjectKind,
        pub properties: BTreeMap<String, WireProperty>,
        pub required: Vec<String>,
    }

    /// 参数模式的根类型。
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "lowercase")]
    pub enum WireObjectKind {
        Object,
    }

    /// 单个参数的 **JSON Schema** 属性。
    #[derive(Debug, Clone, PartialEq, Serialize)]
    pub struct WireProperty {
        #[serde(rename = "type")]
        pub kind: WireType,
        pub description: String,
    }

    /// **JSON Schema** 类型名。
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "lowercase")]
    pub enum WireType {
        String,
        Boolean,
        Integer,
        Number,
        Array,
        Object,
        Null,
    }

    impl WireTool {
        /// 由工具元数据投影工具声明。
        pub fn from_spec(spec: &ToolSpec) -> Self {
            let properties = spec
                .inputs
                .iter()
                .map(|(name, input)| {
                    let property = WireProperty {
                        kind: input.ty.to_wire(),
                        description: input.description.clone(),
                    };
                    (name.clone(), property)
                })
                .collect();
            let required = spec
                .inputs
                .iter()
                .filter(|(_, input)| !input.nullable)
                .map(|(name, _)| name.clone())
                .collect();
            Self {
                kind: WireToolKind::Function,
                function: WireFunction {
                    name: spec.name.clone(),
                    description: spec.description.clone(),
                    parameters: WireParameters {
                        kind: WireObjectKind::Object,
                        properties,
                        required,
                    },
                },
            }
        }
    }
}

/// 判断名称是否为合法标识符：首字符为字母或下划线，其余为字母、数字、下划线或连字符，长度不超过 64。
fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_alphabetic() || first == '_')
        && chars
            .all(|character| character.is_alphanumeric() || character == '_' || character == '-')
        && name.chars().count() <= 64
}

/// 返回 **JSON** 值的类型。
fn value_type(value: &Value) -> InputType {
    match value {
        Value::Null => InputType::Null,
        Value::Bool(_) => InputType::Boolean,
        Value::Number(number) => {
            if number.is_i64() || number.is_u64() {
                InputType::Integer
            } else {
                InputType::Number
            }
        }
        Value::String(_) => InputType::String,
        Value::Array(_) => InputType::Array,
        Value::Object(_) => InputType::Object,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 回显工具：以参数对象作为输出。
    fn echo(arguments: &Map<String, Value>) -> Result<Value, ToolError> {
        Ok(Value::Object(arguments.clone()))
    }

    fn search_spec() -> ToolSpec {
        ToolSpec::new("search", "Performs a web search.", InputType::String)
            .input("query", InputSpec::new(InputType::String, "The search query."))
            .input("limit", InputSpec::new(InputType::Integer, "Maximum results.").nullable())
    }

    fn search_tool() -> FunctionTool<fn(&Map<String, Value>) -> Result<Value, ToolError>> {
        FunctionTool::new(search_spec(), echo)
    }

    #[test]
    fn accepts_identifiers_and_rejects_invalid_names() {
        assert!(is_valid_name("web_search"));
        assert!(is_valid_name("web-search"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("2fast"));
        assert!(!is_valid_name("has space"));
        assert!(!is_valid_name(&"a".repeat(65)));
    }

    #[test]
    fn insert_rejects_invalid_spec() {
        let mut tools = ToolSet::new();
        let invalid = FunctionTool::new(ToolSpec::new("search", "", InputType::String), echo);

        assert!(matches!(tools.insert(invalid), Err(ToolError::InvalidSpec { .. })));
    }

    #[test]
    fn insert_rejects_duplicate_names() {
        let mut tools = ToolSet::new();
        tools.insert(search_tool()).expect("first insertion should succeed");

        assert_eq!(
            tools.insert(search_tool()),
            Err(ToolError::DuplicateName {
                name: "search".into()
            })
        );
    }

    #[tokio::test]
    async fn call_reports_unknown_tool() {
        let tools = ToolSet::new();

        assert_eq!(
            tools.call("search", &json!({})).await,
            Err(ToolError::UnknownTool {
                name: "search".into()
            })
        );
    }

    #[tokio::test]
    async fn call_resolves_and_executes() {
        let mut tools = ToolSet::new();
        tools.insert(search_tool()).expect("insertion should succeed");

        let output =
            tools.call("search", &json!({ "query": "rust" })).await.expect("call should succeed");

        assert_eq!(output, json!({ "query": "rust" }));
    }

    #[tokio::test]
    async fn call_reports_execution_failure() {
        let mut tools = ToolSet::new();
        let failing = FunctionTool::new(
            ToolSpec::new("boom", "Always fails.", InputType::Null),
            |_: &Map<String, Value>| {
                Err(ToolError::ExecutionFailed {
                    name: "boom".into(),
                    reason: "kaboom".into(),
                })
            },
        );
        tools.insert(failing).expect("insertion should succeed");

        let error = tools.call("boom", &json!({})).await.expect_err("call should fail");

        assert_eq!(error.to_string(), "tool 'boom' failed: kaboom");
    }

    #[tokio::test]
    async fn asynchronous_tools_are_awaited_inside_the_registry() {
        struct SlowTool {
            spec: ToolSpec,
        }

        impl Tool for SlowTool {
            fn spec(&self) -> &ToolSpec {
                &self.spec
            }

            fn invoke<'a>(&'a self, arguments: &'a Map<String, Value>) -> ToolFuture<'a> {
                Box::pin(async move {
                    tokio::task::yield_now().await;
                    Ok(arguments.get("text").cloned().unwrap_or(Value::Null))
                })
            }
        }

        let mut tools = ToolSet::new();
        tools
            .insert(SlowTool {
                spec: ToolSpec::new("slow", "Waits, then echoes.", InputType::String)
                    .input("text", InputSpec::new(InputType::String, "Text to echo.")),
            })
            .expect("insertion should succeed");

        let output =
            tools.call("slow", &json!({ "text": "echo" })).await.expect("call should succeed");

        assert_eq!(output, json!("echo"));
    }

    #[test]
    fn resolve_rejects_unknown_argument() {
        let error = resolve_arguments(&search_spec(), &json!({ "query": "rust", "engine": "ddg" }))
            .expect_err("unknown argument should be rejected");

        assert_eq!(
            error,
            ToolError::InvalidArguments {
                name: "search".into(),
                reason: "unknown argument 'engine'".into()
            }
        );
    }

    #[test]
    fn resolve_rejects_missing_required_argument() {
        let error = resolve_arguments(&search_spec(), &json!({ "limit": 3 }))
            .expect_err("missing argument should be rejected");

        assert_eq!(
            error,
            ToolError::InvalidArguments {
                name: "search".into(),
                reason: "missing required argument 'query'".into()
            }
        );
    }

    #[test]
    fn resolve_accepts_missing_and_null_for_nullable_inputs() {
        let missing = resolve_arguments(&search_spec(), &json!({ "query": "rust" }));
        let explicit_null =
            resolve_arguments(&search_spec(), &json!({ "query": "rust", "limit": null }));

        assert!(missing.is_ok());
        assert_eq!(
            explicit_null.expect("null should be accepted").get("limit"),
            Some(&Value::Null)
        );
    }

    #[test]
    fn resolve_rejects_null_for_required_input() {
        let error = resolve_arguments(&search_spec(), &json!({ "query": null }))
            .expect_err("null should be rejected");

        assert_eq!(
            error,
            ToolError::InvalidArguments {
                name: "search".into(),
                reason: "argument 'query' must not be null".into()
            }
        );
    }

    #[test]
    fn resolve_rejects_type_mismatch() {
        let error = resolve_arguments(&search_spec(), &json!({ "query": "rust", "limit": "many" }))
            .expect_err("type mismatch should be rejected");

        assert_eq!(
            error,
            ToolError::InvalidArguments {
                name: "search".into(),
                reason: "argument 'limit' has type 'string' but should be 'integer'".into(),
            }
        );
    }

    #[test]
    fn resolve_widens_integer_to_number() {
        let spec = ToolSpec::new("scale", "Scales a value.", InputType::Number)
            .input("factor", InputSpec::new(InputType::Number, "Scale factor."));

        assert!(resolve_arguments(&spec, &json!({ "factor": 2 })).is_ok());
    }

    #[test]
    fn resolve_accepts_string_for_image_input() {
        let spec = ToolSpec::new("look", "Describes an image.", InputType::String)
            .input("image", InputSpec::new(InputType::Image, "The image to describe."));

        assert!(resolve_arguments(&spec, &json!({ "image": "image.png" })).is_ok());
    }

    #[test]
    fn resolve_treats_scalar_as_single_input() {
        let spec = ToolSpec::new("upper", "Uppercases text.", InputType::String)
            .input("text", InputSpec::new(InputType::String, "The text."));

        let resolved = resolve_arguments(&spec, &json!("rust")).expect("scalar should be accepted");

        assert_eq!(resolved, Map::from_iter([("text".into(), json!("rust"))]));
    }

    #[test]
    fn resolve_requires_object_for_multi_input_tools() {
        let error = resolve_arguments(&search_spec(), &json!("rust"))
            .expect_err("scalar should be rejected");

        assert_eq!(
            error,
            ToolError::InvalidArguments {
                name: "search".into(),
                reason: "arguments must be an object".into()
            }
        );
    }

    #[test]
    fn schema_projects_types_and_required_fields() {
        let spec = ToolSpec::new("render", "Renders an image.", InputType::Image)
            .input("prompt", InputSpec::new(InputType::String, "The prompt."))
            .input("size", InputSpec::new(InputType::Integer, "Edge size.").nullable())
            .input("source", InputSpec::new(InputType::Image, "An image."));

        let json = serde_json::to_value(wire::WireTool::from_spec(&spec))
            .expect("serialization should succeed");

        assert_eq!(
            json,
            json!({
                "type": "function",
                "function": {
                    "name": "render",
                    "description": "Renders an image.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "prompt": { "type": "string", "description": "The prompt." },
                            "size": { "type": "integer", "description": "Edge size." },
                            "source": { "type": "string", "description": "An image." },
                        },
                        "required": ["prompt", "source"],
                    },
                },
            })
        );
    }

    #[test]
    fn set_projects_schemas_and_descriptions_in_name_order() {
        let mut tools = ToolSet::new();
        tools.insert(search_tool()).expect("insertion should succeed");
        tools
            .insert(FunctionTool::new(
                ToolSpec::new("alpha", "First tool.", InputType::String),
                echo,
            ))
            .expect("insertion should succeed");

        let schemas = tools.schemas();
        let names: Vec<&str> = schemas.iter().map(|tool| tool.function.name.as_str()).collect();

        assert_eq!(names, ["alpha", "search"]);
        assert!(tools.describe().starts_with("alpha: First tool."));
    }

    #[test]
    fn describe_renders_inputs_and_output() {
        let description = search_spec().describe();

        assert!(description.starts_with("search: Performs a web search."));
        assert!(description.contains("- limit (integer): Maximum results. [optional]"));
        assert!(description.contains("- query (string): The search query."));
        assert!(description.ends_with("Returns an output of type: string"));
    }
}
