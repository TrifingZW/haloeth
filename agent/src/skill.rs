//! **Skill**：Claude 风格的目录包。
//!
//! 每个 Skill 是一个目录，包含 `SKILL.md`：frontmatter 声明名称、描述与触发条件，
//! 正文是给模型的完整指令；目录中的其他文件是可选资源。
//!
//! 渐进披露：系统提示只放元数据，模型需要时通过 `load_skill` 读取正文与资源清单。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::ToolError;
use crate::tool::{ApprovalRequirement, InputSpec, InputType, Tool, ToolFuture, ToolSpec};

/// Skill 清单文件名。
pub const SKILL_FILE: &str = "SKILL.md";

/// 加载工具的名称。
pub const LOAD_SKILL: &str = "load_skill";

/// 资源清单的最大条目数。
const MAX_RESOURCES: usize = 50;

/// frontmatter 中的字段。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillMeta {
    /// 名称；缺省时使用目录名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 一句话描述。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// 何时使用。
    #[serde(default, rename = "when_to_use", skip_serializing_if = "String::is_empty")]
    pub when_to_use: String,
}

/// 一个已发现的 Skill。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// 名称。
    pub name: String,
    /// 描述。
    pub description: String,
    /// 触发条件。
    pub when_to_use: String,
    /// 正文。
    pub body: String,
    /// 目录。
    pub root: PathBuf,
}

impl Skill {
    /// 系统提示中的一行摘要。
    pub fn summary(&self) -> String {
        let mut line = format!("- {}: {}", self.name, self.description);
        if !self.when_to_use.is_empty() {
            line.push_str(&format!(" (use when: {})", self.when_to_use));
        }
        line
    }

    /// 资源清单：目录中除 `SKILL.md` 之外的文件名。
    pub fn resources(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                (name != SKILL_FILE).then_some(name)
            })
            .collect();
        names.sort();
        names.truncate(MAX_RESOURCES);
        names
    }
}

/// 解析 `SKILL.md`：可选的 YAML frontmatter 加正文。
pub fn parse_skill(text: &str) -> Result<(SkillMeta, String), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let trimmed = text.trim_start();
    if !trimmed.starts_with("---") {
        return Ok((SkillMeta::default(), text.trim().to_owned()));
    }
    let rest = trimmed.strip_prefix("---").unwrap_or(trimmed);
    let rest = rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n')).unwrap_or(rest);
    let Some((frontmatter, body)) = split_frontmatter(rest) else {
        return Err("SKILL.md 的 frontmatter 没有结束标记".to_owned());
    };
    let meta: SkillMeta = serde_yaml_ng::from_str(frontmatter)
        .map_err(|error| format!("frontmatter 解析失败：{error}"))?;
    Ok((meta, body.trim().to_owned()))
}

/// 分离 frontmatter 与正文。
fn split_frontmatter(rest: &str) -> Option<(&str, &str)> {
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.trim() == "---" {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

/// 扫描目录发现 Skill；目录不存在时跳过。
pub fn discover(dirs: &[PathBuf]) -> Vec<Skill> {
    let mut skills: Vec<Skill> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut candidates: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        candidates.sort();
        for root in candidates {
            let Some(skill) = read_skill(&root) else {
                continue;
            };
            // 同名时以先发现的目录为准。
            if skills.iter().any(|existing| existing.name == skill.name) {
                continue;
            }
            skills.push(skill);
        }
    }
    skills
}

/// 读取单个 Skill 目录。
fn read_skill(root: &Path) -> Option<Skill> {
    let text = std::fs::read_to_string(root.join(SKILL_FILE)).ok()?;
    let (meta, body) = parse_skill(&text).ok()?;
    let fallback = root.file_name().map(|name| name.to_string_lossy().to_string())?;
    let name = meta.name.filter(|name| !name.trim().is_empty()).unwrap_or(fallback);
    Some(Skill {
        name,
        description: meta.description,
        when_to_use: meta.when_to_use,
        body,
        root: root.to_path_buf(),
    })
}

/// 按启用列表筛选；启用列表为空时全部启用。
pub fn select(mut skills: Vec<Skill>, enabled: &[String]) -> Vec<Skill> {
    if !enabled.is_empty() {
        skills.retain(|skill| enabled.iter().any(|name| name == &skill.name));
    }
    skills
}

/// 在指定目录下创建一个 Skill 骨架，返回目录路径。
pub fn create_skill(root: &Path, name: &str, description: &str) -> std::io::Result<PathBuf> {
    let dir = root.join(safe_dir_name(name));
    std::fs::create_dir_all(&dir)?;
    let skill_file = dir.join(SKILL_FILE);
    if !skill_file.exists() {
        let description = description.trim();
        let text = format!(
            "---\nname: {name}\ndescription: {description}\nwhen_to_use: \n---\n\n\
             Describe here how the agent should complete tasks with this skill.\n\
             Mention any scripts or resources stored next to this file.\n"
        );
        std::fs::write(skill_file, text)?;
    }
    Ok(dir)
}

/// 把名称转为安全的目录名。
fn safe_dir_name(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '-' | '_' | ' ') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "skill".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `load_skill` 工具：按需返回 Skill 正文与资源清单。
pub struct LoadSkillTool {
    spec: ToolSpec,
    skills: Vec<Skill>,
}

impl LoadSkillTool {
    /// 以已启用的 Skill 构造工具。
    pub fn new(skills: Vec<Skill>) -> Self {
        let spec = ToolSpec::new(
            LOAD_SKILL,
            "Loads a skill's full instructions and lists its resources. Call it when a task matches a skill listed in the system prompt.",
            InputType::String,
        )
        .input("name", InputSpec::new(InputType::String, "Name of the skill to load."))
        .approval(ApprovalRequirement::Never);
        Self {
            spec,
            skills,
        }
    }
}

impl Tool for LoadSkillTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke<'a>(&'a self, arguments: &'a Map<String, Value>) -> ToolFuture<'a> {
        Box::pin(async move {
            let name = arguments.get("name").and_then(Value::as_str).unwrap_or_default();
            let Some(skill) = self.skills.iter().find(|skill| skill.name == name) else {
                let available: Vec<&str> =
                    self.skills.iter().map(|skill| skill.name.as_str()).collect();
                return Err(ToolError::InvalidArguments {
                    name: LOAD_SKILL.to_owned(),
                    reason: format!(
                        "unknown skill '{name}'; available skills: {}",
                        available.join(", ")
                    ),
                });
            };
            let resources = skill.resources();
            let mut text = format!(
                "# Skill: {}\n\n{}\n\nDirectory: {}",
                skill.name,
                skill.body,
                skill.root.display()
            );
            if resources.is_empty() {
                text.push_str("\n\nResources: none");
            } else {
                text.push_str("\n\nResources:\n");
                for resource in resources {
                    text.push_str(&format!("- {resource}\n"));
                }
            }
            Ok(Value::String(text))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workspace(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("haloeth-skill-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("workspace should be created");
        dir
    }

    fn write_skill(root: &Path, dir: &str, body: &str) {
        let dir = root.join(dir);
        std::fs::create_dir_all(&dir).expect("skill dir");
        std::fs::write(dir.join(SKILL_FILE), body).expect("skill file");
    }

    #[test]
    fn parses_frontmatter_and_body() {
        let text = "---\nname: pdf\ndescription: Work with PDFs.\nwhen_to_use: the user mentions a PDF\n---\n\nStep one.\n";

        let (meta, body) = parse_skill(text).expect("skill should parse");

        assert_eq!(meta.name.as_deref(), Some("pdf"));
        assert_eq!(meta.description, "Work with PDFs.");
        assert_eq!(meta.when_to_use, "the user mentions a PDF");
        assert_eq!(body, "Step one.");
    }

    #[test]
    fn files_without_frontmatter_are_all_body() {
        let (meta, body) = parse_skill("Just instructions.").expect("skill should parse");

        assert_eq!(meta, SkillMeta::default());
        assert_eq!(body, "Just instructions.");
    }

    #[test]
    fn unterminated_frontmatter_is_reported() {
        assert!(parse_skill("---\nname: x\nbody").is_err());
    }

    #[test]
    fn discovery_skips_duplicates_and_missing_dirs() {
        let root = workspace("discover");
        write_skill(&root, "a", "---\nname: shared\ndescription: A\n---\nA body");
        write_skill(&root, "b", "---\nname: shared\ndescription: B\n---\nB body");
        write_skill(&root, "c", "---\ndescription: C\n---\nC body");

        let skills = discover(&[root.join("missing"), root.clone()]);

        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "shared");
        assert_eq!(skills[0].description, "A");
        assert_eq!(skills[1].name, "c", "缺少 name 时使用目录名");
    }

    #[test]
    fn selection_keeps_only_enabled_names() {
        let root = workspace("select");
        write_skill(&root, "a", "---\nname: a\ndescription: A\n---\n");
        write_skill(&root, "b", "---\nname: b\ndescription: B\n---\n");
        let skills = discover(&[root]);

        assert_eq!(select(skills.clone(), &[]).len(), 2);
        assert_eq!(select(skills, &["b".to_owned()]).len(), 1);
    }

    #[test]
    fn load_skill_returns_body_and_resources() {
        let root = workspace("load");
        write_skill(&root, "pdf", "---\nname: pdf\ndescription: PDFs\n---\nUse pdftotext.");
        std::fs::write(root.join("pdf").join("script.sh"), "echo").expect("resource");
        let skills = discover(&[root]);
        let tool = LoadSkillTool::new(skills);

        let output =
            futures_block_on(tool.invoke(&Map::from_iter([("name".to_owned(), json!("pdf"))])));

        let text = output.expect("load should succeed").as_str().expect("text").to_owned();
        assert!(text.contains("Use pdftotext."));
        assert!(text.contains("script.sh"));
    }

    #[test]
    fn load_skill_reports_unknown_names() {
        let root = workspace("unknown");
        write_skill(&root, "pdf", "---\nname: pdf\ndescription: PDFs\n---\n");
        let tool = LoadSkillTool::new(discover(&[root]));

        let error =
            futures_block_on(tool.invoke(&Map::from_iter([("name".to_owned(), json!("nope"))])))
                .expect_err("unknown skill should fail");

        assert!(error.to_string().contains("unknown skill"));
    }

    #[test]
    fn creating_a_skill_writes_a_skeleton() {
        let root = workspace("create");

        let dir = create_skill(&root, "My Skill", "Does things").expect("create");

        let text = std::fs::read_to_string(dir.join(SKILL_FILE)).expect("skeleton");
        assert_eq!(dir.file_name().unwrap(), "My Skill");
        assert!(text.starts_with("---\nname: My Skill\ndescription: Does things\n"));
        assert!(discover(&[root]).len() == 1);
    }

    /// 在同步测试里驱动一个已完成的 future。
    fn futures_block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }
}
