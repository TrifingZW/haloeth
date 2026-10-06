/// 一个可选的模型
#[derive(Clone, PartialEq, Eq)]
pub struct Model {
    id: &'static str,
    name: &'static str,
}

impl Model {
    const fn new(id: &'static str, name: &'static str) -> Self {
        Self {
            id,
            name,
        }
    }

    /// 服务商侧的模型标识
    pub fn id(&self) -> &'static str {
        self.id
    }

    /// 界面展示名称
    pub fn name(&self) -> &'static str {
        self.name
    }
}

/// 内置模型目录；接入服务商后由服务商返回的模型列表替换
pub const CATALOG: &[Model] = &[
    Model::new("claude-sonnet-4-5", "Claude Sonnet 4.5"),
    Model::new("gpt-5-codex", "GPT-5 Codex"),
];

/// 默认模型
pub fn default_model() -> Model {
    CATALOG.first().expect("内置模型目录不为空").clone()
}
