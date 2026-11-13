//! 运行配置：提供商、模型目录与当前模型。
//!
//! 载入顺序为：配置文件 → 环境变量 → 空配置，即界面中保存过的设置优先。
//! 密钥仅存放于本机配置文件与内存，不写入版本库；配置文件为 **TOML** 明文。
//!
//! 当前选择是「提供商 + 模型」一个整体：提供商由所选模型所属关系推出，
//! 因此浏览提供商不会改变运行所用的凭证。

use std::path::{Path, PathBuf};

use agent::approval::ApprovalMode;
use agent::model::{Modality, Protocol};
use serde::{Deserialize, Deserializer, Serialize};

/// **API** 密钥的环境变量名。
pub const API_KEY_ENV: &str = "HALOETH_API_KEY";

/// 接口基地址的环境变量名。
pub const BASE_URL_ENV: &str = "HALOETH_BASE_URL";

/// 默认接口基地址。
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// 配置文件名。
const CONFIG_FILE: &str = "config.toml";

/// 配置目录名。
const CONFIG_DIR: &str = "Haloeth";

/// 环境变量回退所生成的服务商名称。
const ENV_PROVIDER: &str = "环境变量";

/// 内置提供商预设。
pub struct ProviderPreset {
    /// 预设名称。
    pub name: &'static str,
    /// 接口基地址。
    pub base_url: &'static str,
    /// 协议。
    pub protocol: Protocol,
    /// 建议的模型标识。
    pub models: &'static [&'static str],
}

/// 预设附带的媒体模型（图片 / 视频输出）。
fn preset_media_models(name: &str) -> Vec<ModelEntry> {
    let image = |model: &str| ModelEntry {
        name: model.to_owned(),
        enabled: true,
        input: vec![Modality::Text],
        output: vec![Modality::Image],
    };
    let video = |model: &str| ModelEntry {
        name: model.to_owned(),
        enabled: true,
        input: vec![Modality::Text],
        output: vec![Modality::Video],
    };
    let vision = |model: &str| ModelEntry {
        name: model.to_owned(),
        enabled: true,
        input: vec![Modality::Text, Modality::Image],
        output: vec![Modality::Text],
    };
    let asr = |model: &str| ModelEntry {
        name: model.to_owned(),
        enabled: true,
        input: vec![Modality::Audio],
        output: vec![Modality::Text],
    };
    let tts = |model: &str| ModelEntry {
        name: model.to_owned(),
        enabled: true,
        input: vec![Modality::Text],
        output: vec![Modality::Audio],
    };
    match name {
        "智谱" => vec![image("cogview-3-flash"), image("cogview-4")],
        "百炼" => vec![
            image("wanx2.1-t2i-turbo"),
            video("wan2.1-t2v-turbo"),
            vision("qwen3-vl-plus"),
            asr("qwen3-asr-flash"),
            tts("qwen3-tts-flash"),
        ],
        "OpenAI" => {
            vec![image("gpt-image-1"), vision("gpt-4.1-mini"), asr("whisper-1"), tts("tts-1")]
        }
        _ => Vec::new(),
    }
}

/// 预设附带的语音 / 生成工具默认配置。
fn apply_preset_defaults(config: &mut Config, name: &str, provider: &str) {
    let (asr_model, tts_model, voice, image_model, video_model) = match name {
        "百炼" => (
            Some("qwen3-asr-flash"),
            Some("qwen3-tts-flash"),
            Some("Cherry"),
            Some("wanx2.1-t2i-turbo"),
            Some("wan2.1-t2v-turbo"),
        ),
        "OpenAI" => (Some("whisper-1"), Some("tts-1"), Some("alloy"), Some("gpt-image-1"), None),
        "智谱" => (None, None, None, Some("cogview-3-flash"), None),
        _ => (None, None, None, None, None),
    };
    if let Some(model) = asr_model
        && !config.audio.asr.is_configured()
    {
        config.audio.asr.backend = AudioBackend::Cloud;
        config.audio.asr.provider = provider.to_owned();
        config.audio.asr.model = model.to_owned();
        if name == "百炼" {
            config.audio.asr.language = "zh".to_owned();
        }
    }
    if let Some(model) = tts_model
        && !config.audio.tts.is_configured()
    {
        config.audio.tts.backend = AudioBackend::Cloud;
        config.audio.tts.provider = provider.to_owned();
        config.audio.tts.model = model.to_owned();
        config.audio.tts.voice = voice.unwrap_or_default().to_owned();
    }
    if let Some(model) = image_model
        && !config.media.image.is_configured()
    {
        config.media.image.provider = provider.to_owned();
        config.media.image.model = model.to_owned();
    }
    if let Some(model) = video_model
        && !config.media.video.is_configured()
    {
        config.media.video.provider = provider.to_owned();
        config.media.video.model = model.to_owned();
    }
}

/// 常用提供商预设；密钥留空，由用户填写。
pub const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        protocol: Protocol::OpenAiChat,
        models: &["gpt-4.1-mini", "gpt-4.1"],
    },
    ProviderPreset {
        name: "Anthropic",
        base_url: "https://api.anthropic.com/v1",
        protocol: Protocol::AnthropicMessages,
        models: &["claude-sonnet-5", "claude-haiku-5-5"],
    },
    ProviderPreset {
        name: "DeepSeek",
        base_url: "https://api.deepseek.com",
        protocol: Protocol::OpenAiChat,
        models: &["deepseek-flash", "deepseek-chat"],
    },
    ProviderPreset {
        name: "Gemini",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai/",
        protocol: Protocol::OpenAiChat,
        models: &["gemini-2.5-flash", "gemini-2.5-pro"],
    },
    ProviderPreset {
        name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        protocol: Protocol::OpenAiChat,
        models: &["openai/gpt-4o-mini", "anthropic/claude-sonnet-4.5"],
    },
    ProviderPreset {
        name: "硅基流动",
        base_url: "https://api.siliconflow.cn/v1",
        protocol: Protocol::OpenAiChat,
        models: &["Qwen/Qwen3-8B", "deepseek-ai/DeepSeek-V3"],
    },
    ProviderPreset {
        name: "百炼",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        protocol: Protocol::OpenAiChat,
        models: &["qwen-plus", "qwen-max"],
    },
    ProviderPreset {
        name: "智谱",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        protocol: Protocol::OpenAiChat,
        models: &["glm-4.5", "glm-4.5-air"],
    },
    ProviderPreset {
        name: "Moonshot",
        base_url: "https://api.moonshot.cn/v1",
        protocol: Protocol::OpenAiChat,
        models: &["kimi-k2-0905-preview", "moonshot-v1-8k"],
    },
    ProviderPreset {
        name: "Ollama / vLLM",
        base_url: "http://localhost:11434/v1",
        protocol: Protocol::OpenAiChat,
        models: &["llama3.1", "qwen2.5"],
    },
];

/// 服务商：一个接口地址与密钥的组合，可承载多个模型。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    /// 名称，同时作为唯一标识。
    pub name: String,
    /// 接口基地址。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_url: String,
    /// **API** 密钥。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    /// 模型目录，含每个模型支持的输入 / 输出模态。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelEntry>,
    /// 工具选择策略；思考型提供商（如 DeepSeek）需要 `auto`。
    #[serde(default)]
    pub tool_choice: agent::openai::ToolChoicePolicy,
    /// 提供方协议。
    #[serde(default)]
    pub protocol: Protocol,
    /// 附加请求头；同名时覆盖默认头。
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    /// 合并进请求体的额外字段，最后写入、优先级最高。
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub extra_body: std::collections::BTreeMap<String, serde_json::Value>,
    /// 生成的最大 Token 数；仅 Anthropic 使用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

/// 一个模型条目：名称与它支持的输入 / 输出模态。
#[derive(Clone, PartialEq, Eq, Serialize, Debug)]
pub struct ModelEntry {
    /// 模型标识。
    pub name: String,
    /// 是否启用；禁用的模型不出现在任何选择器中。
    #[serde(default = "enabled_by_default", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// 支持的输入模态；空表示文本。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<Modality>,
    /// 支持的输出模态；空表示文本。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<Modality>,
}

impl ModelEntry {
    /// 以名称构造文本→文本模型。
    pub fn text(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            enabled: true,
            input: vec![Modality::Text],
            output: vec![Modality::Text],
        }
    }

    /// 输入模态，缺省为文本。
    pub fn inputs(&self) -> Vec<Modality> {
        normalise_modalities(&self.input)
    }

    /// 输出模态，缺省为文本。
    pub fn outputs(&self) -> Vec<Modality> {
        normalise_modalities(&self.output)
    }

    /// 是否满足要求的输入 / 输出模态。
    pub fn matches(&self, input: &[Modality], output: &[Modality]) -> bool {
        let inputs = self.inputs();
        let outputs = self.outputs();
        input.iter().all(|modality| inputs.contains(modality))
            && output.iter().all(|modality| outputs.contains(modality))
    }
}

/// 模态列表：空表示文本，并按固定顺序去重。
fn normalise_modalities(modalities: &[Modality]) -> Vec<Modality> {
    if modalities.is_empty() {
        return vec![Modality::Text];
    }
    let mut values = modalities.to_vec();
    values.sort();
    values.dedup();
    values
}

/// 兼容旧格式：`models = ["a"]` 或新的完整条目。
#[derive(Deserialize)]
#[serde(untagged)]
enum RawModelEntry {
    Name(String),
    Full {
        name: String,
        #[serde(default = "enabled_by_default")]
        enabled: bool,
        #[serde(default)]
        input: Vec<Modality>,
        #[serde(default)]
        output: Vec<Modality>,
    },
}

impl<'de> Deserialize<'de> for ModelEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match RawModelEntry::deserialize(deserializer)? {
            RawModelEntry::Name(name) => Self::text(name),
            RawModelEntry::Full {
                name,
                enabled,
                input,
                output,
            } => Self {
                name,
                enabled,
                input: normalise_modalities(&input),
                output: normalise_modalities(&output),
            },
        })
    }
}

/// 提供商 + 模型的选择。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ModelSelection {
    /// 提供商名称。
    #[serde(default)]
    pub provider: String,
    /// 模型标识。
    #[serde(default)]
    pub model: String,
}

impl ModelSelection {
    /// 是否已完整填写。
    pub fn is_set(&self) -> bool {
        !self.provider.trim().is_empty() && !self.model.trim().is_empty()
    }
}

impl Provider {
    /// 构造服务商。
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            models: Vec::new(),
            tool_choice: agent::openai::ToolChoicePolicy::default(),
            protocol: Protocol::default(),
            headers: std::collections::BTreeMap::new(),
            extra_body: std::collections::BTreeMap::new(),
            max_tokens: None,
        }
    }

    /// 接口地址，空值时回退到默认值。
    pub fn endpoint(&self) -> String {
        let base_url = self.base_url.trim();
        if base_url.is_empty() {
            DEFAULT_BASE_URL.to_owned()
        } else {
            base_url.to_owned()
        }
    }

    /// 是否已配置密钥。
    pub fn is_configured(&self) -> bool {
        !self.api_key.trim().is_empty()
    }
}

impl std::fmt::Debug for Provider {
    /// 脱敏输出，避免密钥进入日志。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provider")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("models", &self.models.len())
            .field("tool_choice", &self.tool_choice)
            .field("protocol", &self.protocol)
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// 运行配置：提供商列表与当前模型。
#[derive(Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// 全部提供商。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<Provider>,
    /// 当前模型所属的提供商名称。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_provider: Option<String>,
    /// 当前模型标识。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_model: Option<String>,
    /// 审核设置。
    #[serde(default)]
    pub approval: ApprovalConfig,
    /// 搜索设置。
    #[serde(default)]
    pub search: SearchConfig,
    /// 工具设置。
    #[serde(default)]
    pub tools: ToolsConfig,
    /// 存储设置。
    #[serde(default)]
    pub storage: StorageConfig,
    /// MCP 服务器。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<agent::mcp::McpServerConfig>,
    /// Skill 设置。
    #[serde(default)]
    pub skills: SkillsConfig,
    /// 界面设置。
    #[serde(default)]
    pub ui: UiConfig,
    /// 全局功能偏好：显示与默认启用。
    #[serde(default)]
    pub feature_prefs: FeaturePrefs,
    /// 语音设置。
    #[serde(default)]
    pub audio: AudioConfig,
    /// 生成工具设置。
    #[serde(default)]
    pub media: MediaConfig,
}

/// 单个功能的全局偏好。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeaturePref {
    /// 是否在任何地方显示；隐藏后不会加载。
    #[serde(default)]
    pub hidden: bool,
    /// 新建会话时的默认状态，以及重新显示后的初始状态。
    #[serde(default)]
    pub default_enabled: bool,
}

impl FeaturePref {
    /// 构造偏好。
    pub fn new(hidden: bool, default_enabled: bool) -> Self {
        Self {
            hidden,
            default_enabled,
        }
    }
}

/// 功能类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureKind {
    /// 内置工具。
    Tool,
    /// MCP 服务器。
    Mcp,
    /// Skill。
    Skill,
}

impl FeatureKind {
    /// 会话记录中的键前缀。
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Mcp => "mcp",
            Self::Skill => "skill",
        }
    }

    /// 默认是否启用：工具与 MCP 是，Skill 不是。
    pub fn default_enabled(self) -> bool {
        !matches!(self, Self::Skill)
    }
}

/// 全部功能的全局偏好。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeaturePrefs {
    /// 内置工具。
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub tools: std::collections::BTreeMap<String, FeaturePref>,
    /// MCP 服务器。
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub mcp: std::collections::BTreeMap<String, FeaturePref>,
    /// Skill。
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub skills: std::collections::BTreeMap<String, FeaturePref>,
}

impl FeaturePrefs {
    /// 指定类别的偏好表。
    pub fn map(&self, kind: FeatureKind) -> &std::collections::BTreeMap<String, FeaturePref> {
        match kind {
            FeatureKind::Tool => &self.tools,
            FeatureKind::Mcp => &self.mcp,
            FeatureKind::Skill => &self.skills,
        }
    }

    /// 指定类别的可变偏好表。
    pub fn map_mut(
        &mut self,
        kind: FeatureKind,
    ) -> &mut std::collections::BTreeMap<String, FeaturePref> {
        match kind {
            FeatureKind::Tool => &mut self.tools,
            FeatureKind::Mcp => &mut self.mcp,
            FeatureKind::Skill => &mut self.skills,
        }
    }

    /// 读取一条偏好；缺失时使用类别默认值。
    pub fn get(&self, kind: FeatureKind, name: &str) -> FeaturePref {
        if let Some(pref) = self.map(kind).get(name) {
            return pref.clone();
        }
        if kind == FeatureKind::Tool && MediaTool::from_tool_name(name).is_some() {
            return FeaturePref::new(true, false);
        }
        FeaturePref::new(false, kind.default_enabled())
    }

    /// 是否显示。
    pub fn is_visible(&self, kind: FeatureKind, name: &str) -> bool {
        !self.get(kind, name).hidden
    }

    /// 新会话的默认状态；隐藏的功能视为禁用。
    pub fn default_state(&self, kind: FeatureKind, name: &str) -> bool {
        let pref = self.get(kind, name);
        !pref.hidden && pref.default_enabled
    }

    /// 写入一条偏好。
    pub fn set(&mut self, kind: FeatureKind, name: &str, pref: FeaturePref) {
        self.map_mut(kind).insert(name.to_owned(), pref);
    }

    /// 切换显示状态，返回新值。
    pub fn toggle_hidden(&mut self, kind: FeatureKind, name: &str) -> bool {
        let mut pref = self.get(kind, name);
        pref.hidden = !pref.hidden;
        let hidden = pref.hidden;
        self.set(kind, name, pref);
        hidden
    }

    /// 切换默认启用状态，返回新值。
    pub fn toggle_default(&mut self, kind: FeatureKind, name: &str) -> bool {
        let mut pref = self.get(kind, name);
        pref.default_enabled = !pref.default_enabled;
        let enabled = pref.default_enabled;
        self.set(kind, name, pref);
        enabled
    }
}

/// 语音后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioBackend {
    /// 关闭。
    #[default]
    Off,
    /// 云端模型。
    Cloud,
    /// 系统自带能力（Windows 听写 / SAPI）。
    System,
}

impl AudioBackend {
    /// 配置中的取值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Cloud => "cloud",
            Self::System => "system",
        }
    }

    /// 界面展示文本。
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "关闭",
            Self::Cloud => "云端模型",
            Self::System => "系统自带",
        }
    }
}

/// 语音设置。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioConfig {
    /// 语音识别。
    #[serde(default)]
    pub asr: AsrConfig,
    /// 语音合成。
    #[serde(default)]
    pub tts: TtsConfig,
}

impl AsrConfig {
    /// 是否已填写提供商与模型。
    pub fn is_configured(&self) -> bool {
        !self.provider.trim().is_empty() && !self.model.trim().is_empty()
    }
}

impl TtsConfig {
    /// 是否已填写提供商与模型。
    pub fn is_configured(&self) -> bool {
        !self.provider.trim().is_empty() && !self.model.trim().is_empty()
    }
}

/// 语音识别设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsrConfig {
    /// 后端。
    #[serde(default)]
    pub backend: AudioBackend,
    /// 提供商名称。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    /// 模型标识；须支持 音频输入 + 文本输出。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// 识别语言；空表示自动。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub language: String,
    /// 麦克风设备名；空表示系统默认。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub input_device: String,
    /// 转写接口地址；空表示按提供商推导。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub endpoint: String,
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            backend: AudioBackend::Off,
            provider: String::new(),
            model: String::new(),
            language: String::new(),
            input_device: String::new(),
            endpoint: String::new(),
        }
    }
}

/// 语音合成设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TtsConfig {
    /// 后端。
    #[serde(default)]
    pub backend: AudioBackend,
    /// 提供商名称。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    /// 模型标识；须支持 文本输入 + 音频输出。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// 系统语音名称；空表示默认。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub voice: String,
    /// 语速，-10..10。
    #[serde(default)]
    pub rate: i32,
    /// 音量，0..100。
    #[serde(default = "default_volume")]
    pub volume: u8,
    /// 语音合成接口地址；空表示按提供商推导。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub endpoint: String,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            backend: AudioBackend::Off,
            provider: String::new(),
            model: String::new(),
            voice: String::new(),
            rate: 0,
            volume: default_volume(),
            endpoint: String::new(),
        }
    }
}

/// 默认音量。
fn default_volume() -> u8 {
    100
}

/// 生成工具设置。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaConfig {
    /// 绘图。
    #[serde(default)]
    pub image: MediaToolConfig,
    /// 视频。
    #[serde(default)]
    pub video: MediaToolConfig,
    /// 音乐。
    #[serde(default)]
    pub music: MediaToolConfig,
}

impl MediaConfig {
    /// 生成工具是否已经配置了可用的模型。
    pub fn ready(&self, config: &Config, kind: MediaTool) -> bool {
        let media = self.tool(kind);
        if !media.is_configured() {
            return false;
        }
        config
            .model_entry(media.provider.trim(), media.model.trim())
            .is_some_and(|entry| entry.enabled && entry.matches(&[kind.input()], &[kind.output()]))
    }

    /// 指定工具的设置。
    pub fn tool(&self, kind: MediaTool) -> &MediaToolConfig {
        match kind {
            MediaTool::Image => &self.image,
            MediaTool::Video => &self.video,
            MediaTool::Music => &self.music,
        }
    }

    /// 指定工具的可变设置。
    pub fn tool_mut(&mut self, kind: MediaTool) -> &mut MediaToolConfig {
        match kind {
            MediaTool::Image => &mut self.image,
            MediaTool::Video => &mut self.video,
            MediaTool::Music => &mut self.music,
        }
    }
}

/// 生成工具类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaTool {
    /// 绘图。
    Image,
    /// 视频生成。
    Video,
    /// 音乐生成。
    Music,
}

impl MediaTool {
    /// 全部类别。
    pub const ALL: [Self; 3] = [Self::Image, Self::Video, Self::Music];

    /// 从工具名解析生成工具。
    pub fn from_tool_name(name: &str) -> Option<Self> {
        match name {
            "generate_image" => Some(Self::Image),
            "generate_video" => Some(Self::Video),
            "generate_music" => Some(Self::Music),
            _ => None,
        }
    }

    /// 工具名。
    pub fn tool_name(self) -> &'static str {
        match self {
            Self::Image => "generate_image",
            Self::Video => "generate_video",
            Self::Music => "generate_music",
        }
    }

    /// 界面名称。
    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "AI 绘图",
            Self::Video => "AI 视频",
            Self::Music => "AI 音乐",
        }
    }

    /// 输入模态：都是文本。
    pub fn input(self) -> Modality {
        Modality::Text
    }

    /// 输出模态。
    pub fn output(self) -> Modality {
        match self {
            Self::Image => Modality::Image,
            Self::Video => Modality::Video,
            Self::Music => Modality::Audio,
        }
    }
}

/// 单个生成工具的设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaToolConfig {
    /// 提供商名称。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    /// 模型标识。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// 接口地址；空表示由提供商推导。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub endpoint: String,
    /// 附加请求头（例如百炼的 `X-DashScope-Async`）。
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    /// 任务提交请求体模板；字符串中的 `{{prompt}}` 会被替换。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_template: Option<serde_json::Value>,
    /// 轮询地址模板，支持 `{{base}}`、`{{id}}`。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub poll_url: String,
    /// 任务标识字段路径，点号分隔。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id_path: String,
    /// 状态字段路径。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status_path: String,
    /// 表示完成的状态值。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub done_value: String,
    /// 结果地址字段路径。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub result_path: String,
    /// 轮询间隔（毫秒）。
    #[serde(default = "default_poll_interval")]
    pub interval_ms: u64,
    /// 超时（秒）。
    #[serde(default = "default_job_timeout")]
    pub timeout_s: u64,
}

/// 默认轮询间隔。
fn default_poll_interval() -> u64 {
    2000
}

/// 默认任务超时。
fn default_job_timeout() -> u64 {
    300
}

impl Default for MediaToolConfig {
    fn default() -> Self {
        Self {
            provider: String::new(),
            model: String::new(),
            endpoint: String::new(),
            headers: Default::default(),
            body_template: None,
            poll_url: String::new(),
            id_path: "id".to_owned(),
            status_path: "status".to_owned(),
            done_value: "succeeded".to_owned(),
            result_path: "output.url".to_owned(),
            interval_ms: default_poll_interval(),
            timeout_s: default_job_timeout(),
        }
    }
}

impl MediaToolConfig {
    /// 是否已完整配置。
    pub fn is_configured(&self) -> bool {
        !self.provider.trim().is_empty() && !self.model.trim().is_empty()
    }
}

/// 界面设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiConfig {
    /// 配色方案。
    #[serde(default)]
    pub theme: ThemePreference,
    /// 发送消息的按键偏好。
    #[serde(default)]
    pub send_key: SendKey,
    /// 输入框是否显示行号。
    #[serde(default)]
    pub line_numbers: bool,
    /// 关闭主窗口后继续在后台运行。
    #[serde(default = "enabled_by_default")]
    pub run_in_background: bool,
    /// 按住说话快捷键，例如 `ctrl+v`；空表示关闭。
    #[serde(default = "default_hotkey")]
    pub voice_hotkey: String,
    /// 通用提示词，附加到每个对话的系统提示。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub system_prompt: String,
}

/// 默认的按住说话快捷键。
fn default_hotkey() -> String {
    "ctrl+v".to_owned()
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: ThemePreference::default(),
            send_key: SendKey::default(),
            line_numbers: false,
            run_in_background: true,
            voice_hotkey: default_hotkey(),
            system_prompt: String::new(),
        }
    }
}

/// 发送消息的按键偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SendKey {
    /// **Enter** 发送，**Shift+Enter** 换行。
    #[default]
    Enter,
    /// **Ctrl+Enter**（macOS 为 **⌘+Enter**）发送，**Enter** 换行。
    SecondaryEnter,
}

impl SendKey {
    /// 配置中的取值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::SecondaryEnter => "secondary-enter",
        }
    }

    /// 界面展示文本。
    pub fn label(self) -> &'static str {
        match self {
            Self::Enter => "Enter 发送",
            Self::SecondaryEnter => "Ctrl+Enter 发送",
        }
    }

    /// 是否把普通 **Enter** 当作发送。
    pub fn sends_on_enter(self) -> bool {
        matches!(self, Self::Enter)
    }
}

/// 配色方案偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    /// 跟随系统。
    #[default]
    System,
    /// 浅色。
    Light,
    /// 深色。
    Dark,
}

impl ThemePreference {
    /// 配置中的取值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// 界面展示文本。
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "跟随系统",
            Self::Light => "浅色",
            Self::Dark => "深色",
        }
    }
}

/// Skill 设置。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsConfig {
    /// 启用的 Skill 名称；为空表示全部启用。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled: Vec<String>,
    /// 额外的 Skill 根目录。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<String>,
}

/// 存储设置。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageConfig {
    /// 会话目录；为空时使用系统默认目录。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sessions_dir: String,
    /// 上次打开的会话标识符。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_session: String,
}

/// 审核设置。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalConfig {
    /// 审核模式。
    #[serde(default)]
    pub mode: ApprovalMode,
    /// 规则模式下是否自动放行公网网络访问。
    #[serde(default)]
    pub allow_network: bool,
    /// 审核模型设置。
    #[serde(default)]
    pub reviewer: ReviewerConfig,
}

impl Default for ApprovalConfig {
    fn default() -> Self {
        Self {
            mode: ApprovalMode::Rules,
            allow_network: false,
            reviewer: ReviewerConfig::default(),
        }
    }
}

impl std::fmt::Debug for ApprovalConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalConfig")
            .field("mode", &self.mode)
            .field("allow_network", &self.allow_network)
            .field("reviewer", &self.reviewer)
            .finish()
    }
}

/// 审核模型设置：可以复用当前对话模型，也可以单独指定。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerConfig {
    /// 是否使用当前对话模型。
    #[serde(default)]
    pub use_current: bool,
    /// 单独指定的提供商名称。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    /// 单独指定的模型标识。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
}

impl Default for ReviewerConfig {
    fn default() -> Self {
        Self {
            use_current: true,
            provider: String::new(),
            model: String::new(),
        }
    }
}

impl std::fmt::Debug for ReviewerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReviewerConfig")
            .field("use_current", &self.use_current)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .finish()
    }
}

/// 搜索设置。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchConfig {
    /// 是否启用 `web_search`。
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// 绑定的提供商名称；为空时使用当前对话提供商。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    /// 接口地址；为空时由提供商基地址推导。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub endpoint: String,
    /// 接口密钥；为空时复用提供商密钥。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    /// 默认返回结果数。
    #[serde(default = "default_search_results")]
    pub max_results: usize,
}

/// 是否显式写为禁用。
fn is_true(value: &bool) -> bool {
    *value
}

/// 序列化时的默认值：搜索默认启用。
fn enabled_by_default() -> bool {
    true
}

/// 搜索默认结果数。
fn default_search_results() -> usize {
    5
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: String::new(),
            endpoint: String::new(),
            api_key: String::new(),
            max_results: default_search_results(),
        }
    }
}

impl std::fmt::Debug for SearchConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchConfig")
            .field("enabled", &self.enabled)
            .field("provider", &self.provider)
            .field("endpoint", &self.endpoint)
            .field("api_key", &"<redacted>")
            .field("max_results", &self.max_results)
            .finish()
    }
}

/// 工具设置。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsConfig {
    /// 需要关闭的工具名称。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled: Vec<String>,
}

impl std::fmt::Debug for Config {
    /// 脱敏输出，避免密钥进入日志。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("providers", &self.providers)
            .field("active_provider", &self.active_provider)
            .field("active_model", &self.active_model)
            .field("approval", &self.approval)
            .field("search", &self.search)
            .field("tools", &self.tools)
            .field("storage", &self.storage)
            .field("mcp_servers", &self.mcp_servers)
            .field("skills", &self.skills)
            .field("ui", &self.ui)
            .finish()
    }
}

impl Config {
    /// 载入配置：配置文件优先，其次由环境变量回退，最后为空配置。
    pub fn load() -> Self {
        let mut config = match config_path().and_then(|path| read(&path)) {
            Some(config) => config,
            None => Self::from_env(),
        };
        config.normalise();
        config
    }

    /// 以环境变量构造回退配置；未设置密钥时为空配置。
    fn from_env() -> Self {
        let api_key = std::env::var(API_KEY_ENV).unwrap_or_default();
        if api_key.trim().is_empty() {
            return Self::default();
        }
        let base_url = std::env::var(BASE_URL_ENV).unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned());
        Self {
            providers: vec![Provider::new(ENV_PROVIDER, base_url, api_key)],
            active_provider: Some(ENV_PROVIDER.to_owned()),
            active_model: None,
            ..Self::default()
        }
    }

    /// Skill 根目录：用户追加的目录在前，公认的 `~/.agents/skills` 在后。
    pub fn skill_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self
            .skills
            .dirs
            .iter()
            .map(|dir| PathBuf::from(dir.trim()))
            .filter(|dir| !dir.as_os_str().is_empty())
            .collect();
        if let Some(base) = config_dir() {
            // 旧版本的目录仍然读取，避免升级后已有 Skill 消失。
            let legacy = base.join("skills");
            if legacy.is_dir() {
                dirs.push(legacy);
            }
        }
        if let Some(home) = home_dir() {
            dirs.push(home.join(".agents").join("skills"));
        }
        dirs
    }

    /// 默认的 Skill 创建目录：`~/.agents/skills`。
    pub fn default_skill_dir(&self) -> Option<PathBuf> {
        home_dir().map(|home| home.join(".agents").join("skills"))
    }

    /// 会话目录：优先使用自定义路径，否则使用系统默认目录。
    pub fn sessions_root(&self) -> PathBuf {
        let custom = self.storage.sessions_dir.trim();
        if custom.is_empty() {
            crate::session_store::SessionStore::default_root()
        } else {
            PathBuf::from(custom)
        }
    }

    /// 配置文件路径；无法确定用户配置目录时返回 [`None`]。
    pub fn path() -> Option<PathBuf> {
        config_path()
    }

    /// 保存到配置文件。
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = config_path() else {
            return Err(std::io::Error::other("找不到配置目录"));
        };
        self.save_to(&path)
    }

    /// 当前提供商：当前模型所属的提供商。
    pub fn provider(&self) -> Option<&Provider> {
        self.provider_named(self.active_provider.as_deref()?)
    }

    /// 按名称查找提供商。
    pub fn provider_named(&self, name: &str) -> Option<&Provider> {
        self.providers.iter().find(|provider| provider.name == name)
    }

    /// 当前模型标识。
    pub fn model(&self) -> Option<&str> {
        self.active_model.as_deref()
    }

    /// 当前选择：提供商名称与模型标识。
    pub fn selection(&self) -> Option<(&str, &str)> {
        Some((self.provider()?.name.as_str(), self.model()?))
    }

    /// 列出满足输入 / 输出模态要求的模型。
    pub fn models_with(&self, input: &[Modality], output: &[Modality]) -> Vec<ModelSelection> {
        self.providers
            .iter()
            .flat_map(|provider| {
                provider
                    .models
                    .iter()
                    .filter(|model| model.enabled && model.matches(input, output))
                    .map(|model| ModelSelection {
                        provider: provider.name.clone(),
                        model: model.name.clone(),
                    })
            })
            .collect()
    }

    /// 查找模型条目。
    pub fn model_entry(&self, provider: &str, model: &str) -> Option<&ModelEntry> {
        self.provider_named(provider)?.models.iter().find(|entry| entry.name == model)
    }

    /// 把旧版开关迁移到 `feature_prefs`；只在首次升级时执行。
    fn migrate_legacy_features(&mut self) {
        if !self.feature_prefs.tools.is_empty()
            || !self.feature_prefs.mcp.is_empty()
            || !self.feature_prefs.skills.is_empty()
        {
            return;
        }
        for name in std::mem::take(&mut self.tools.disabled) {
            self.feature_prefs.set(FeatureKind::Tool, &name, FeaturePref::new(false, false));
        }
        for server in &self.mcp_servers {
            self.feature_prefs.set(
                FeatureKind::Mcp,
                &server.name,
                FeaturePref::new(false, server.enabled),
            );
        }
        for name in std::mem::take(&mut self.skills.enabled) {
            self.feature_prefs.set(FeatureKind::Skill, &name, FeaturePref::new(false, true));
        }
    }

    /// 规整当前选择：保持仍然有效的选择，否则退回当前提供商的第一个模型，再退回第一个有模型的提供商。
    pub fn normalise(&mut self) {
        self.migrate_legacy_features();
        if self.selection_is_available() {
            return;
        }
        if let Some(model) =
            self.provider().and_then(|provider| provider.models.iter().find(|model| model.enabled))
        {
            self.active_model = Some(model.name.clone());
            return;
        }
        match self
            .providers
            .iter()
            .find(|provider| provider.models.iter().any(|model| model.enabled))
        {
            Some(provider) => {
                self.active_provider = Some(provider.name.clone());
                self.active_model = provider
                    .models
                    .iter()
                    .find(|model| model.enabled)
                    .map(|model| model.name.clone());
            }
            None => {
                self.active_provider = self.providers.first().map(|provider| provider.name.clone());
                self.active_model = None;
            }
        }
    }

    /// 当前选择是否仍然可用：提供商存在且目录包含当前模型。
    fn selection_is_available(&self) -> bool {
        self.provider().is_some_and(|provider| {
            self.model().is_some_and(|model| {
                provider.models.iter().any(|candidate| candidate.name == model && candidate.enabled)
            })
        })
    }

    /// 按预设新增提供商；名称重复时追加序号。
    pub fn add_preset(&mut self, preset: &ProviderPreset) -> Result<(), String> {
        let name = self.unique_provider_name(preset.name);
        let mut provider = Provider::new(name.clone(), preset.base_url, "");
        provider.protocol = preset.protocol;
        provider.models = preset.models.iter().map(|model| ModelEntry::text(*model)).collect();
        // 预设的媒体模型与文本模型可能重名（如 gpt-4.1-mini 同时支持图像输入），
        // 重名时合并模态，避免出现两个同标识的条目。
        for media in preset_media_models(preset.name) {
            match provider.models.iter_mut().find(|entry| entry.name == media.name) {
                Some(existing) => {
                    for modality in media.input {
                        if !existing.input.contains(&modality) {
                            existing.input.push(modality);
                        }
                    }
                    for modality in media.output {
                        if !existing.output.contains(&modality) {
                            existing.output.push(modality);
                        }
                    }
                }
                None => provider.models.push(media),
            }
        }
        let provider_name = name.clone();
        self.add_provider(provider)?;
        apply_preset_defaults(self, preset.name, &provider_name);
        Ok(())
    }

    /// 生成不重复的提供商名称。
    fn unique_provider_name(&self, base: &str) -> String {
        if self.provider_named(base).is_none() {
            return base.to_owned();
        }
        (2..)
            .map(|index| format!("{base} {index}"))
            .find(|name| self.provider_named(name).is_none())
            .unwrap_or_else(|| base.to_owned())
    }

    /// 新增提供商；名称重复时报错，当前选择保持不变。
    pub fn add_provider(&mut self, provider: Provider) -> Result<(), String> {
        let mut provider = provider;
        provider.name = provider.name.trim().to_owned();
        if provider.name.is_empty() {
            return Err("请填写提供商名称".to_owned());
        }
        if self.provider_named(&provider.name).is_some() {
            return Err(format!("提供商「{}」已存在", provider.name));
        }
        self.providers.push(provider);
        self.normalise();
        Ok(())
    }

    /// 更新服务商的接口地址与密钥。
    pub fn update_provider(&mut self, name: &str, base_url: String, api_key: String) {
        if let Some(provider) = self.providers.iter_mut().find(|provider| provider.name == name) {
            provider.base_url = base_url;
            provider.api_key = api_key;
        }
        self.normalise();
    }

    /// 重命名服务商；名称重复时报错。
    pub fn rename_provider(&mut self, from: &str, to: &str) -> Result<(), String> {
        let to = to.trim().to_owned();
        if to.is_empty() {
            return Err("请填写提供商名称".to_owned());
        }
        if from == to {
            return Ok(());
        }
        if self.provider_named(&to).is_some() {
            return Err(format!("提供商「{to}」已存在"));
        }
        let Some(provider) = self.providers.iter_mut().find(|provider| provider.name == from)
        else {
            return Err(format!("提供商「{from}」不存在"));
        };
        provider.name = to.clone();
        if self.active_provider.as_deref() == Some(from) {
            self.active_provider = Some(to.clone());
        }
        // 其它引用同一提供商的功能也要跟着改名，否则它们会静默失效。
        for media in [&mut self.media.image, &mut self.media.video, &mut self.media.music] {
            if media.provider.trim() == from {
                media.provider = to.clone();
            }
        }
        for audio in [&mut self.audio.asr.provider, &mut self.audio.tts.provider] {
            if audio.trim() == from {
                *audio = to.clone();
            }
        }
        if self.search.provider.trim() == from {
            self.search.provider = to.clone();
        }
        if self.approval.reviewer.provider.trim() == from {
            self.approval.reviewer.provider = to.clone();
        }
        Ok(())
    }

    /// 删除提供商并规整选择。
    pub fn remove_provider(&mut self, name: &str) {
        self.providers.retain(|provider| provider.name != name);
        self.normalise();
    }

    /// 为当前提供商添加模型标识；重复或为空时忽略。
    pub fn add_model(&mut self, provider: &str, model: &str) {
        let model = model.trim().to_owned();
        if model.is_empty() {
            return;
        }
        if let Some(target) = self.providers.iter_mut().find(|entry| entry.name == provider)
            && !target.models.iter().any(|candidate| candidate.name == model)
        {
            target.models.push(ModelEntry::text(model));
            target.models.sort_by(|left, right| left.name.cmp(&right.name));
        }
        self.normalise();
    }

    /// 从指定提供商删除模型标识并规整选择。
    pub fn remove_model(&mut self, provider: &str, model: &str) {
        if let Some(target) = self.providers.iter_mut().find(|entry| entry.name == provider) {
            target.models.retain(|candidate| candidate.name != model);
        }
        self.normalise();
    }

    /// 替换指定服务商的模型目录（拉取结果）并规整选择。
    pub fn replace_models(&mut self, provider: &str, models: Vec<String>) {
        if let Some(target) = self.providers.iter_mut().find(|entry| entry.name == provider) {
            // 保留已有模型的模态设置，新出现的模型按文本处理。
            let existing: std::collections::BTreeMap<String, ModelEntry> =
                target.models.iter().map(|entry| (entry.name.clone(), entry.clone())).collect();
            target.models = models
                .into_iter()
                .map(|name| existing.get(&name).cloned().unwrap_or_else(|| ModelEntry::text(name)))
                .collect();
            target.models.sort_by(|left, right| left.name.cmp(&right.name));
        }
        self.normalise();
    }

    /// 清除当前选择（「无默认模型」）。
    pub fn clear_selection(&mut self) {
        self.active_provider = None;
        self.active_model = None;
    }

    /// 选中指定提供商的模型。
    pub fn select_model(&mut self, provider: &str, model: &str) -> Result<(), String> {
        let Some(target) = self.provider_named(provider) else {
            return Err(format!("提供商「{provider}」不存在"));
        };
        if !target.models.iter().any(|candidate| candidate.name == model && candidate.enabled) {
            return Err(format!("模型「{model}」不在提供商「{provider}」的可用目录中"));
        }
        self.active_provider = Some(provider.to_owned());
        self.active_model = Some(model.to_owned());
        Ok(())
    }

    /// 写入指定路径。
    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(path, format!("{}\n", text.trim_end()))
    }
}

/// 读取配置文件；文件缺失或格式错误时按未配置处理。
fn read(path: &Path) -> Option<Config> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

/// 用户主目录。
pub fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

/// 配置目录：`APPDATA`（Windows）、`XDG_CONFIG_HOME`、`HOME/.config` 依次尝试。
fn config_dir() -> Option<PathBuf> {
    if let Some(base) = std::env::var_os("APPDATA") {
        return Some(PathBuf::from(base).join(CONFIG_DIR));
    }
    if let Some(base) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(base).join(CONFIG_DIR.to_lowercase()));
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".config").join(CONFIG_DIR.to_lowercase()))
}

/// 配置文件路径。
fn config_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(CONFIG_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("haloeth-config-{name}.toml"))
    }

    fn provider(name: &str, models: &[&str]) -> Provider {
        let mut provider = Provider::new(name, "https://example.com/v1", "secret");
        provider.models = models.iter().map(|model| ModelEntry::text(*model)).collect();
        provider
    }

    #[test]
    fn renaming_a_provider_updates_every_reference() {
        let mut config = Config::default();
        let mut provider = Provider::new("百炼", "https://example.com/v1", "key");
        provider.models = vec![ModelEntry::text("qwen")];
        config.add_provider(provider).expect("provider");
        config.select_model("百炼", "qwen").expect("select");
        config.media.image.provider = "百炼".into();
        config.audio.asr.provider = "百炼".into();
        config.audio.tts.provider = "百炼".into();
        config.search.provider = "百炼".into();
        config.approval.reviewer.provider = "百炼".into();
        config.approval.reviewer.model = "qwen".into();

        config.rename_provider("百炼", "百炼 2").expect("rename");

        assert_eq!(config.selection(), Some(("百炼 2", "qwen")));
        assert_eq!(config.media.image.provider, "百炼 2");
        assert_eq!(config.audio.asr.provider, "百炼 2");
        assert_eq!(config.audio.tts.provider, "百炼 2");
        assert_eq!(config.search.provider, "百炼 2");
        assert_eq!(config.approval.reviewer.provider, "百炼 2");
    }

    #[test]
    fn presets_do_not_duplicate_model_ids() {
        let mut config = Config::default();
        let openai =
            PROVIDER_PRESETS.iter().find(|preset| preset.name == "OpenAI").expect("preset");
        config.add_preset(openai).expect("add preset");
        let provider = config.provider_named("OpenAI").expect("provider");
        let mut names: Vec<&str> =
            provider.models.iter().map(|model| model.name.as_str()).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "预设不应产生重复的模型标识");

        // 重名的视觉模型合并输入模态，而不是新增一条。
        let vision =
            provider.models.iter().find(|model| model.name == "gpt-4.1-mini").expect("model");
        assert!(vision.inputs().contains(&Modality::Image));
    }

    #[test]
    fn presets_fill_protocol_and_models() {
        let mut config = Config::default();
        let anthropic =
            PROVIDER_PRESETS.iter().find(|preset| preset.name == "Anthropic").expect("preset");
        config.add_preset(anthropic).expect("add preset");
        let provider = config.provider_named("Anthropic").expect("provider");
        assert_eq!(provider.protocol, Protocol::AnthropicMessages);
        assert_eq!(provider.base_url, "https://api.anthropic.com/v1");
        assert_eq!(provider.models[0].inputs(), vec![Modality::Text]);
        config.add_preset(anthropic).expect("duplicate preset");
        assert!(config.provider_named("Anthropic 2").is_some());

        let zhipu = PROVIDER_PRESETS.iter().find(|preset| preset.name == "智谱").expect("preset");
        config.add_preset(zhipu).expect("zhipu");
        let provider = config.provider_named("智谱").expect("provider");
        assert!(
            provider
                .models
                .iter()
                .any(|model| model.name == "cogview-3-flash"
                    && model.output == vec![Modality::Image])
        );
    }

    #[test]
    fn protocols_headers_and_audio_round_trip() {
        let path = temp_path("protocols");
        let mut config = Config::default();
        let mut entry = provider("gateway", &["m"]);
        entry.protocol = Protocol::AnthropicMessages;
        entry.headers.insert("X-Tenant".into(), "team".into());
        entry.max_tokens = Some(2048);
        config.add_provider(entry).expect("provider");
        config.audio.asr.backend = AudioBackend::Cloud;
        config.audio.tts.backend = AudioBackend::System;
        config.audio.tts.rate = 3;
        config.media.image.provider = "gateway".into();
        config.media.image.model = "m".into();

        config.save_to(&path).expect("save");
        let restored = read(&path).expect("read");

        assert_eq!(restored.providers[0].protocol, Protocol::AnthropicMessages);
        let text = std::fs::read_to_string(&path).expect("read text");
        assert!(text.contains("protocol = \"anthropic-messages\""));
        assert_eq!(restored.providers[0].headers.get("X-Tenant").map(String::as_str), Some("team"));
        assert_eq!(restored.providers[0].max_tokens, Some(2048));
        assert_eq!(restored.audio.asr.backend, AudioBackend::Cloud);
        assert_eq!(restored.audio.tts.backend, AudioBackend::System);
        assert_eq!(restored.audio.tts.rate, 3);
        assert!(restored.media.image.is_configured());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn provider_endpoint_falls_back_to_the_default() {
        assert_eq!(Provider::new("local", "  ", "secret").endpoint(), DEFAULT_BASE_URL);
        assert_eq!(
            Provider::new("local", "https://example.com/v1", "secret").endpoint(),
            "https://example.com/v1"
        );
    }

    #[test]
    fn provider_requires_a_non_blank_key() {
        assert!(!Provider::new("local", "https://api.test", "  ").is_configured());
        assert!(Provider::new("local", "https://api.test", "secret").is_configured());
    }

    #[test]
    fn the_first_catalog_with_models_becomes_active() {
        let mut config = Config::default();
        config.add_provider(provider("空目录", &[])).expect("新增应当成功");
        config.add_provider(provider("有模型", &["b2", "b1"])).expect("新增应当成功");

        assert_eq!(config.active_provider.as_deref(), Some("有模型"));
        assert_eq!(config.model(), Some("b2"));
    }

    #[test]
    fn adding_a_provider_keeps_the_current_model() {
        let mut config = Config::default();
        config.add_provider(provider("a", &["m1"])).expect("新增应当成功");

        config.add_provider(provider("b", &["m2"])).expect("新增应当成功");

        assert_eq!(config.selection(), Some(("a", "m1")));
    }

    #[test]
    fn an_empty_catalog_clears_the_active_model() {
        let mut config = Config::default();
        config.add_provider(provider("空目录", &[])).expect("新增应当成功");

        assert_eq!(config.active_provider.as_deref(), Some("空目录"));
        assert_eq!(config.model(), None);
    }

    #[test]
    fn adding_a_model_selects_it_when_the_catalog_was_empty() {
        let mut config = Config::default();
        config.add_provider(provider("空目录", &[])).expect("新增应当成功");

        config.add_model("空目录", "m1");

        assert_eq!(config.selection(), Some(("空目录", "m1")));
    }

    #[test]
    fn removing_the_active_provider_falls_back_to_another_catalog() {
        let mut config = Config::default();
        config.add_provider(provider("a", &["m1"])).expect("新增应当成功");
        config.add_provider(provider("b", &["m2"])).expect("新增应当成功");

        config.remove_provider("a");

        assert_eq!(config.selection(), Some(("b", "m2")));
    }

    #[test]
    fn catalog_changes_keep_the_active_model_when_possible() {
        let mut config = Config::default();
        config.add_provider(provider("p", &["m1", "m2"])).expect("新增应当成功");
        config.select_model("p", "m2").expect("选中应当成功");

        config.replace_models("p", vec!["m2".to_owned(), "m3".to_owned()]);
        assert_eq!(config.model(), Some("m2"));

        config.replace_models("p", vec!["m3".to_owned()]);
        assert_eq!(config.model(), Some("m3"));

        config.remove_model("p", "m3");
        assert_eq!(config.model(), None);
    }

    #[test]
    fn duplicate_provider_names_are_rejected() {
        let mut config = Config::default();
        config.add_provider(provider("p", &[])).expect("新增应当成功");

        assert_eq!(config.add_provider(provider("p", &[])), Err("提供商「p」已存在".to_owned()));
        assert_eq!(config.add_provider(provider("  ", &[])), Err("请填写提供商名称".to_owned()));
    }

    #[test]
    fn renaming_a_provider_moves_the_active_selection() {
        let mut config = Config::default();
        config.add_provider(provider("旧名", &["m1"])).expect("新增应当成功");

        config.rename_provider("旧名", "新名").expect("重命名应当成功");

        assert_eq!(config.active_provider.as_deref(), Some("新名"));
        assert_eq!(config.provider().map(|provider| provider.name.as_str()), Some("新名"));
    }

    #[test]
    fn renaming_onto_an_existing_name_is_rejected() {
        let mut config = Config::default();
        config.add_provider(provider("a", &[])).expect("新增应当成功");
        config.add_provider(provider("b", &[])).expect("新增应当成功");

        assert_eq!(config.rename_provider("a", "b"), Err("提供商「b」已存在".to_owned()));
    }

    #[test]
    fn selecting_an_unknown_model_is_rejected() {
        let mut config = Config::default();
        config.add_provider(provider("p", &["m1"])).expect("新增应当成功");

        assert_eq!(
            config.select_model("p", "m9"),
            Err("模型「m9」不在提供商「p」的可用目录中".to_owned())
        );
        assert_eq!(config.select_model("缺失", "m1"), Err("提供商「缺失」不存在".to_owned()));
    }

    #[test]
    fn selecting_a_model_switches_the_active_provider() {
        let mut config = Config::default();
        config.add_provider(provider("a", &["m1"])).expect("新增应当成功");
        config.add_provider(provider("b", &["m2"])).expect("新增应当成功");

        config.select_model("b", "m2").expect("选中应当成功");

        assert_eq!(config.selection(), Some(("b", "m2")));
        assert_eq!(config.provider().map(|provider| provider.name.as_str()), Some("b"));
    }

    #[test]
    fn mcp_and_skill_settings_round_trip_through_toml() {
        let path = temp_path("mcp");
        let mut config = Config::default();
        config.mcp_servers.push(agent::mcp::McpServerConfig {
            name: "files".into(),
            enabled: true,
            transport: agent::mcp::McpTransport::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "server".into()],
                env: std::collections::BTreeMap::new(),
            },
        });
        config.skills.enabled.push("pdf".into());

        config.save_to(&path).expect("保存应当成功");
        let restored = read(&path).expect("应当能读回配置");

        assert_eq!(restored.mcp_servers, config.mcp_servers);
        assert_eq!(restored.skills, config.skills);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ui_preferences_round_trip_and_default_to_enter() {
        let path = temp_path("send-key");
        let mut config = Config::default();
        config.ui.theme = ThemePreference::Dark;
        config.ui.send_key = SendKey::SecondaryEnter;
        let mut entry = provider("deepseek", &["deepseek-flash"]);
        entry.tool_choice = agent::openai::ToolChoicePolicy::Required;
        config.add_provider(entry).expect("新增应当成功");

        config.save_to(&path).expect("保存应当成功");
        let restored = read(&path).expect("应当能读回配置");

        assert_eq!(restored.ui.theme, ThemePreference::Dark);
        assert_eq!(restored.ui.send_key, SendKey::SecondaryEnter);
        assert_eq!(restored.providers[0].tool_choice, agent::openai::ToolChoicePolicy::Required);
        let text = std::fs::read_to_string(&path).expect("读取配置");
        assert!(text.contains("theme = \"dark\""));
        assert!(text.contains("send_key = \"secondary-enter\""));
        assert!(text.contains("tool_choice = \"required\""));
        let _ = std::fs::remove_file(&path);

        // 旧配置没有 `send_key` 时回退到 Enter 发送。
        let legacy = temp_path("send-key-legacy");
        std::fs::write(&legacy, "[ui]\ntheme = \"system\"\n").expect("写入旧配置");
        assert_eq!(read(&legacy).expect("应当能读回配置").ui.send_key, SendKey::Enter);
        let _ = std::fs::remove_file(&legacy);
    }

    #[test]
    fn configuration_round_trips_through_toml() {
        let path = temp_path("round-trip");
        let mut config = Config::default();
        config.add_provider(provider("a", &["m2", "m1"])).expect("新增应当成功");
        config.add_provider(provider("b", &["x"])).expect("新增应当成功");
        config.select_model("a", "m1").expect("选中应当成功");

        config.save_to(&path).expect("保存应当成功");
        let restored = read(&path).expect("应当能读回配置");

        assert_eq!(restored, config);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn configuration_file_is_written_as_toml() {
        let path = temp_path("format");
        let mut config = Config::default();
        config.add_provider(provider("OpenAI", &["gpt-5-codex"])).expect("新增应当成功");

        config.save_to(&path).expect("保存应当成功");
        let text = std::fs::read_to_string(&path).expect("应当能读回配置");

        assert!(text.contains("active_provider = \"OpenAI\""));
        assert!(text.contains("[[providers]]"));
        assert!(text.contains("api_key = \"secret\""));
        assert!(text.contains("[[providers.models]]"));
        assert!(text.contains("name = \"gpt-5-codex\""));
        assert!(text.contains("input = [\"text\"]"));
        assert!(text.ends_with('\n'));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_model_lists_and_feature_flags_migrate() {
        let config: Config = toml::from_str(
            "active_provider = \"p\"\nactive_model = \"old\"\n\
             [[providers]]\nname = \"p\"\napi_key = \"k\"\nmodels = [\"old\"]\n",
        )
        .expect("legacy config should parse");
        assert_eq!(config.providers[0].models, vec![ModelEntry::text("old")]);
        assert!(config.providers[0].models[0].matches(&[Modality::Text], &[Modality::Text]));

        let mut legacy = Config::default();
        legacy.tools.disabled = vec!["shell".into()];
        legacy.skills.enabled = vec!["pdf".into()];
        legacy.mcp_servers.push(agent::mcp::McpServerConfig {
            name: "files".into(),
            enabled: false,
            transport: agent::mcp::McpTransport::Stdio {
                command: "npx".into(),
                args: Vec::new(),
                env: Default::default(),
            },
        });
        legacy.normalise();

        assert!(!legacy.feature_prefs.default_state(FeatureKind::Tool, "shell"));
        assert!(legacy.feature_prefs.default_state(FeatureKind::Tool, "read_file"));
        assert!(legacy.feature_prefs.default_state(FeatureKind::Skill, "pdf"));
        assert!(!legacy.feature_prefs.default_state(FeatureKind::Skill, "other"));
        assert!(!legacy.feature_prefs.default_state(FeatureKind::Mcp, "files"));
        assert!(legacy.tools.disabled.is_empty());
        assert!(legacy.skills.enabled.is_empty());
    }

    #[test]
    fn skills_default_to_the_agents_directory() {
        let config = Config::default();
        let dirs = config.skill_dirs();
        assert!(dirs.iter().any(|dir| dir.components().any(|part| part.as_os_str() == ".agents")));
        assert!(
            config
                .default_skill_dir()
                .is_some_and(|dir| dir.components().any(|part| part.as_os_str() == ".agents"))
        );
    }

    #[test]
    fn ui_preferences_include_background_hotkey_and_prompt() {
        let path = temp_path("ui-extra");
        let mut config = Config::default();
        config.ui.run_in_background = false;
        config.ui.voice_hotkey = "ctrl+shift+v".into();
        config.ui.system_prompt = "始终用简体中文回答".into();
        config.save_to(&path).expect("save");
        let restored = read(&path).expect("read");
        assert!(!restored.ui.run_in_background);
        assert_eq!(restored.ui.voice_hotkey, "ctrl+shift+v");
        assert_eq!(restored.ui.system_prompt, "始终用简体中文回答");
        let _ = std::fs::remove_file(&path);
        assert_eq!(Config::default().ui.voice_hotkey, "ctrl+v");
        assert!(Config::default().ui.run_in_background);
    }

    #[test]
    fn media_tools_are_hidden_and_disabled_by_default() {
        let config = Config::default();
        assert!(config.feature_prefs.get(FeatureKind::Tool, "generate_image").hidden);
        assert!(!config.feature_prefs.default_state(FeatureKind::Tool, "generate_image"));
        assert!(!config.feature_prefs.get(FeatureKind::Tool, "generate_image").default_enabled);
        assert!(config.feature_prefs.default_state(FeatureKind::Tool, "read_file"));
    }

    #[test]
    fn models_are_filtered_by_input_and_output_modalities() {
        let mut config = Config::default();
        let mut entry = Provider::new("p", "https://example.com/v1", "k");
        entry.models = vec![
            ModelEntry::text("chat"),
            ModelEntry {
                name: "vision".into(),
                enabled: true,
                input: vec![Modality::Text, Modality::Image],
                output: vec![Modality::Text],
            },
            ModelEntry {
                name: "painter".into(),
                enabled: true,
                input: vec![Modality::Text],
                output: vec![Modality::Image],
            },
            ModelEntry {
                name: "speech".into(),
                enabled: true,
                input: vec![Modality::Audio],
                output: vec![Modality::Text],
            },
        ];
        config.add_provider(entry).expect("provider");

        let chat = config.models_with(&[Modality::Text], &[Modality::Text]);
        assert!(chat.iter().any(|entry| entry.model == "chat"));
        assert!(chat.iter().any(|entry| entry.model == "vision"));
        assert!(!chat.iter().any(|entry| entry.model == "painter"));

        let image = config.models_with(&[Modality::Text], &[Modality::Image]);
        assert_eq!(image.len(), 1);
        assert_eq!(image[0].model, "painter");

        let asr = config.models_with(&[Modality::Audio], &[Modality::Text]);
        assert_eq!(asr.len(), 1);
        assert_eq!(asr[0].model, "speech");
    }

    #[test]
    fn empty_configuration_writes_only_the_header_fields() {
        let path = temp_path("empty");

        Config::default().save_to(&path).expect("保存应当成功");
        let text = std::fs::read_to_string(&path).expect("应当能读回配置");

        assert!(!text.contains("providers"));
        assert!(!text.contains("active_provider"));

        let _ = std::fs::remove_file(&path);
    }
}
