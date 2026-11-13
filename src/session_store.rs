//! 会话文件：多会话的创建、读取、保存与列表。
//!
//! 存储位置由前端决定（默认系统数据目录，可自定义）；代理内核只认识 [`ContextSnapshot`]。
//! 每个会话一个 JSON 文件，写入采用「同目录临时文件 + 原子替换」，避免中断留下半个文件。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use agent::approval::{AllowRule, DirectoryGrant};
use agent::metrics::TokenUsage;
use agent::persist::ContextSnapshot;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::conversation::TranscriptEntry;

/// 会话文件格式版本。
pub const SESSION_VERSION: u32 = 1;

/// 默认会话标题。
pub const DEFAULT_TITLE: &str = "新会话";

/// 一个会话的全部持久化状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFile {
    /// 格式版本。
    pub version: u32,
    /// 会话标识符，同时是文件名。
    pub id: String,
    /// 标题。
    pub title: String,
    /// 创建时间（Unix 毫秒）。
    pub created_at: u64,
    /// 最后更新时间（Unix 毫秒）。
    pub updated_at: u64,
    /// 该会话使用的提供商；为空时跟随全局配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// 该会话使用的模型；为空时跟随全局配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 会话级目录授权。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<DirectoryGrant>,
    /// 会话级「始终允许」规则。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<AllowRule>,
    /// 会话级功能状态：键为 `tool:name` / `mcp:name` / `skill:name`，缺省用全局默认值。
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub features: std::collections::BTreeMap<String, bool>,
    /// 显示记录。
    #[serde(default)]
    pub transcript: Vec<TranscriptEntry>,
    /// 执行上下文。
    #[serde(default)]
    pub context: ContextSnapshot,
    /// 累计 **Token** 用量。
    #[serde(default)]
    pub usage: TokenUsage,
    /// 按模型拆分的 **Token** 用量。
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub usage_by_model: std::collections::BTreeMap<String, TokenUsage>,
}

impl SessionFile {
    /// 创建一个空会话。
    pub fn new(provider: Option<String>, model: Option<String>) -> Self {
        let now = now_millis();
        Self {
            version: SESSION_VERSION,
            id: Uuid::new_v4().to_string(),
            title: DEFAULT_TITLE.to_owned(),
            created_at: now,
            updated_at: now,
            provider,
            model,
            grants: Vec::new(),
            rules: Vec::new(),
            features: std::collections::BTreeMap::new(),
            transcript: Vec::new(),
            context: ContextSnapshot::default(),
            usage: TokenUsage::default(),
            usage_by_model: std::collections::BTreeMap::new(),
        }
    }

    /// 文本消息数量。
    pub fn message_count(&self) -> usize {
        self.transcript.iter().filter(|entry| matches!(entry, TranscriptEntry::Text { .. })).count()
    }

    /// 以第一条用户消息生成默认标题；已有自定义标题时保持不变。
    pub fn suggest_title(&mut self) {
        if self.title != DEFAULT_TITLE && !self.title.trim().is_empty() {
            return;
        }
        let Some(first) = self.transcript.iter().find_map(|entry| match entry {
            TranscriptEntry::Text {
                role: crate::conversation::ChatRole::User,
                text,
            } => Some(text),
            _ => None,
        }) else {
            return;
        };
        let title: String = first.chars().take(40).collect();
        self.title = title.trim().to_owned();
        if self.title.is_empty() {
            self.title = DEFAULT_TITLE.to_owned();
        }
    }
}

/// 会话列表项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// 会话标识符。
    pub id: String,
    /// 标题。
    pub title: String,
    /// 最后更新时间（Unix 毫秒）。
    pub updated_at: u64,
    /// 文本消息数量。
    pub message_count: usize,
}

/// 会话目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    /// 以指定根目录构造。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
        }
    }

    /// 默认根目录：系统数据目录下的 `Haloeth/sessions`。
    pub fn default_root() -> PathBuf {
        directories::BaseDirs::new()
            .map(|dirs| dirs.data_local_dir().join("Haloeth").join("sessions"))
            .unwrap_or_else(|| std::env::temp_dir().join("Haloeth").join("sessions"))
    }

    /// 根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 指定会话的文件路径。
    pub fn path_for(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }

    /// 创建一个会话并立即写入磁盘。
    pub fn create(
        &self,
        provider: Option<String>,
        model: Option<String>,
    ) -> Result<SessionFile, String> {
        let mut file = SessionFile::new(provider, model);
        self.save(&mut file)?;
        Ok(file)
    }

    /// 读取一个会话。
    pub fn load(&self, id: &str) -> Result<SessionFile, String> {
        let path = self.path_for(id);
        let text = fs::read_to_string(&path).map_err(|error| format!("读取会话失败：{error}"))?;
        serde_json::from_str(&text).map_err(|error| format!("会话文件损坏：{error}"))
    }

    /// 保存会话；刷新更新时间并原子替换文件。
    pub fn save(&self, file: &mut SessionFile) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|error| format!("创建会话目录失败：{error}"))?;
        file.updated_at = now_millis();
        let text =
            serde_json::to_string_pretty(file).map_err(|error| format!("序列化失败：{error}"))?;

        let mut temp = tempfile::NamedTempFile::new_in(&self.root)
            .map_err(|error| format!("创建临时文件失败：{error}"))?;
        temp.write_all(text.as_bytes()).map_err(|error| format!("写入失败：{error}"))?;
        temp.as_file().sync_all().map_err(|error| format!("落盘失败：{error}"))?;
        temp.persist(self.path_for(&file.id))
            .map_err(|error| format!("替换会话文件失败：{}", error.error))?;
        Ok(())
    }

    /// 列出全部可读会话，按更新时间倒序。
    pub fn list(&self) -> Vec<SessionSummary> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut summaries: Vec<SessionSummary> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| {
                let text = fs::read_to_string(entry.path()).ok()?;
                let file: SessionFile = serde_json::from_str(&text).ok()?;
                let message_count = file.message_count();
                Some(SessionSummary {
                    id: file.id,
                    title: file.title,
                    updated_at: file.updated_at,
                    message_count,
                })
            })
            .collect();
        summaries.sort_by(|left, right| {
            right.updated_at.cmp(&left.updated_at).then_with(|| right.id.cmp(&left.id))
        });
        summaries
    }

    /// 重命名会话。
    pub fn rename(&self, id: &str, title: &str) -> Result<SessionFile, String> {
        let mut file = self.load(id)?;
        let title = title.trim();
        if !title.is_empty() {
            file.title = title.to_owned();
        }
        self.save(&mut file)?;
        Ok(file)
    }

    /// 删除会话。
    pub fn delete(&self, id: &str) -> Result<(), String> {
        fs::remove_file(self.path_for(id)).map_err(|error| format!("删除会话失败：{error}"))
    }
}

/// 当前时间（Unix 毫秒）。
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{ChatRole, TranscriptEntry};

    fn store(name: &str) -> SessionStore {
        let root = std::env::temp_dir().join(format!("haloeth-sessions-{name}"));
        let _ = fs::remove_dir_all(&root);
        SessionStore::new(root)
    }

    #[test]
    fn sessions_round_trip_through_disk() {
        let store = store("round-trip");
        let mut file = store.create(Some("p".into()), Some("m".into())).expect("create");
        file.transcript.push(TranscriptEntry::Text {
            role: ChatRole::User,
            text: "列出当前目录".into(),
        });
        store.save(&mut file).expect("save");

        let loaded = store.load(&file.id).expect("load");

        assert_eq!(loaded.id, file.id);
        assert_eq!(loaded.transcript, file.transcript);
        assert_eq!(loaded.provider.as_deref(), Some("p"));
    }

    #[test]
    fn listing_sorts_by_update_time_descending() {
        let store = store("list");
        let mut older = store.create(None, None).expect("create");
        older.title = "旧".into();
        older.updated_at = 10;
        store.save(&mut older).expect("save");
        let mut newer = store.create(None, None).expect("create");
        newer.title = "新".into();
        newer.updated_at = 20;
        store.save(&mut newer).expect("save");

        let list = store.list();

        assert_eq!(list.len(), 2);
        assert_eq!(list[0].title, "新");
        assert_eq!(list[1].title, "旧");
    }

    #[test]
    fn corrupt_files_are_skipped() {
        let store = store("corrupt");
        fs::create_dir_all(store.root()).expect("root");
        fs::write(store.path_for("broken"), "not json").expect("write");

        assert!(store.list().is_empty());
        assert!(store.load("broken").is_err());
    }

    #[test]
    fn saving_replaces_the_previous_file() {
        let store = store("replace");
        let mut file = store.create(None, None).expect("create");
        file.title = "第一次".into();
        store.save(&mut file).expect("save");
        file.title = "第二次".into();
        store.save(&mut file).expect("save");

        assert_eq!(store.load(&file.id).expect("load").title, "第二次");
        let entries: Vec<_> =
            fs::read_dir(store.root()).expect("root").filter_map(Result::ok).collect();
        assert_eq!(entries.len(), 1, "不应留下临时文件");
    }

    #[test]
    fn renaming_and_deleting_work() {
        let store = store("rename");
        let file = store.create(None, None).expect("create");

        let renamed = store.rename(&file.id, "新标题").expect("rename");
        assert_eq!(renamed.title, "新标题");
        store.delete(&file.id).expect("delete");
        assert!(store.list().is_empty());
    }

    #[test]
    fn titles_are_suggested_from_the_first_user_message() {
        let mut file = SessionFile::new(None, None);
        file.transcript.push(TranscriptEntry::Text {
            role: ChatRole::User,
            text: "帮我整理一下下载目录里的截图".into(),
        });

        file.suggest_title();

        assert_eq!(file.title, "帮我整理一下下载目录里的截图");
        file.title = "自定义".into();
        file.suggest_title();
        assert_eq!(file.title, "自定义");
    }

    #[test]
    fn message_counts_ignore_activity_rows() {
        let mut file = SessionFile::new(None, None);
        file.transcript.push(TranscriptEntry::Text {
            role: ChatRole::User,
            text: "任务".into(),
        });

        assert_eq!(file.message_count(), 1);
    }
}
