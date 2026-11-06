//! 运行配置：提供商、模型目录与当前模型。
//!
//! 载入顺序为：配置文件 → 环境变量 → 空配置，即界面中保存过的设置优先。
//! 密钥仅存放于本机配置文件与内存，不写入版本库；配置文件为 **TOML** 明文。
//!
//! 当前选择是「提供商 + 模型」一个整体：提供商由所选模型所属关系推出，
//! 因此浏览提供商不会改变运行所用的凭证。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    /// 模型标识目录。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
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
            .finish()
    }
}

/// 运行配置：提供商列表与当前模型。
#[derive(Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl std::fmt::Debug for Config {
    /// 脱敏输出，避免密钥进入日志。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("providers", &self.providers)
            .field("active_provider", &self.active_provider)
            .field("active_model", &self.active_model)
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

    /// 规整当前选择：保持仍然有效的选择，否则退回当前提供商的第一个模型，再退回第一个有模型的提供商。
    pub fn normalise(&mut self) {
        if self.selection_is_available() {
            return;
        }
        if let Some(model) = self.provider().and_then(|provider| provider.models.first()) {
            self.active_model = Some(model.clone());
            return;
        }
        match self.providers.iter().find(|provider| !provider.models.is_empty()) {
            Some(provider) => {
                self.active_provider = Some(provider.name.clone());
                self.active_model = provider.models.first().cloned();
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
            self.model()
                .is_some_and(|model| provider.models.iter().any(|candidate| candidate == model))
        })
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
            self.active_provider = Some(to);
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
            && !target.models.iter().any(|candidate| candidate == &model)
        {
            target.models.push(model);
            target.models.sort();
        }
        self.normalise();
    }

    /// 从指定提供商删除模型标识并规整选择。
    pub fn remove_model(&mut self, provider: &str, model: &str) {
        if let Some(target) = self.providers.iter_mut().find(|entry| entry.name == provider) {
            target.models.retain(|candidate| candidate != model);
        }
        self.normalise();
    }

    /// 替换指定服务商的模型目录（拉取结果）并规整选择。
    pub fn replace_models(&mut self, provider: &str, models: Vec<String>) {
        if let Some(target) = self.providers.iter_mut().find(|entry| entry.name == provider) {
            target.models = models;
        }
        self.normalise();
    }

    /// 选中指定提供商的模型。
    pub fn select_model(&mut self, provider: &str, model: &str) -> Result<(), String> {
        let Some(target) = self.provider_named(provider) else {
            return Err(format!("提供商「{provider}」不存在"));
        };
        if !target.models.iter().any(|candidate| candidate == model) {
            return Err(format!("模型「{model}」不在提供商「{provider}」的目录中"));
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
        provider.models = models.iter().map(|model| (*model).to_owned()).collect();
        provider
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
            Err("模型「m9」不在提供商「p」的目录中".to_owned())
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
        assert!(text.contains("models = [\"gpt-5-codex\"]"));
        assert!(text.ends_with('\n'));

        let _ = std::fs::remove_file(&path);
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
