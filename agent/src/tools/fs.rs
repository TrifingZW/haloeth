//! 文件工具：读取、写入与目录列举。
//!
//! 路径安全由审核层负责；这些工具只做内容层面的约束：
//! 拒绝二进制文件、限制读取与列举规模、写文件时不自动创建父目录。

use std::path::Path;

use serde_json::{Map, Value};

use crate::error::ToolError;
use crate::text::truncate;
use crate::tool::{Access, InputSpec, InputType, Tool, ToolEffect, ToolFuture, ToolSpec};

/// 读取的最大字符数。
const MAX_READ: usize = 200_000;

/// 列举的最大条目数。
const MAX_ENTRIES: usize = 500;

/// 读取文件。
pub struct ReadFileTool {
    spec: ToolSpec,
}

impl ReadFileTool {
    /// 构造工具。
    pub fn new() -> Self {
        let spec = ToolSpec::new(
            "read_file",
            "Reads a UTF-8 text file. Use offset and limit to page through large files.",
            InputType::String,
        )
        .input("path", InputSpec::new(InputType::String, "Path of the file to read."))
        .input(
            "offset",
            InputSpec::new(InputType::Integer, "First line to read, 1-based.").nullable(),
        )
        .input("limit", InputSpec::new(InputType::Integer, "Maximum lines to read.").nullable())
        .effect(ToolEffect::Path {
            argument: "path".to_owned(),
            access: Access::Read,
        });
        Self {
            spec,
        }
    }
}

impl Default for ReadFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for ReadFileTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke<'a>(&'a self, arguments: &'a Map<String, Value>) -> ToolFuture<'a> {
        Box::pin(async move {
            let path = required_string(arguments, "path")?;
            let bytes = tokio::fs::read(&path).await.map_err(|error| failure(&path, error))?;
            if content_inspector::inspect(&bytes).is_binary() {
                return Err(ToolError::ExecutionFailed {
                    name: "read_file".to_owned(),
                    reason: format!("{path} looks like a binary file"),
                });
            }
            let text = String::from_utf8_lossy(&bytes);
            let offset = integer(arguments, "offset").unwrap_or(1).max(1) as usize;
            let limit = integer(arguments, "limit").map(|limit| limit.max(0) as usize);

            let lines: Vec<&str> = text.lines().collect();
            let start = (offset - 1).min(lines.len());
            let end = match limit {
                Some(limit) => (start + limit).min(lines.len()),
                None => lines.len(),
            };
            let selected = lines[start..end].join("\n");
            let body = truncate(&selected, MAX_READ);

            let mut header = format!("{path}\nlines {}-{} of {}", start + 1, end, lines.len());
            if selected.chars().count() > MAX_READ {
                header.push_str(&format!("\n[truncated to {MAX_READ} characters]"));
            }
            Ok(Value::String(format!("{header}\n\n{body}")))
        })
    }
}

/// 写入文件。
pub struct WriteFileTool {
    spec: ToolSpec,
}

impl WriteFileTool {
    /// 构造工具。
    pub fn new() -> Self {
        let spec = ToolSpec::new(
            "write_file",
            "Writes text to a file, replacing its contents. The parent directory must exist.",
            InputType::String,
        )
        .input("path", InputSpec::new(InputType::String, "Path of the file to write."))
        .input("content", InputSpec::new(InputType::String, "Text to write."))
        .effect(ToolEffect::Path {
            argument: "path".to_owned(),
            access: Access::Write,
        });
        Self {
            spec,
        }
    }
}

impl Default for WriteFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for WriteFileTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke<'a>(&'a self, arguments: &'a Map<String, Value>) -> ToolFuture<'a> {
        Box::pin(async move {
            let path = required_string(arguments, "path")?;
            let content = required_string(arguments, "content")?;
            if let Some(parent) = Path::new(&path).parent()
                && !parent.as_os_str().is_empty()
                && !tokio::fs::try_exists(parent).await.unwrap_or(false)
            {
                return Err(ToolError::ExecutionFailed {
                    name: "write_file".to_owned(),
                    reason: format!("parent directory {} does not exist", parent.display()),
                });
            }
            tokio::fs::write(&path, content.as_bytes())
                .await
                .map_err(|error| failure(&path, error))?;
            Ok(Value::String(format!("wrote {} bytes to {path}", content.len())))
        })
    }
}

/// 列举目录。
pub struct ListDirTool {
    spec: ToolSpec,
}

impl ListDirTool {
    /// 构造工具。
    pub fn new() -> Self {
        let spec = ToolSpec::new(
            "list_dir",
            "Lists the entries of a directory with their kind (file or directory).",
            InputType::String,
        )
        .input("path", InputSpec::new(InputType::String, "Directory to list."))
        .effect(ToolEffect::Path {
            argument: "path".to_owned(),
            access: Access::Read,
        });
        Self {
            spec,
        }
    }
}

impl Default for ListDirTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for ListDirTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke<'a>(&'a self, arguments: &'a Map<String, Value>) -> ToolFuture<'a> {
        Box::pin(async move {
            let path = required_string(arguments, "path")?;
            let mut entries =
                tokio::fs::read_dir(&path).await.map_err(|error| failure(&path, error))?;
            let mut lines = Vec::new();
            let mut total = 0_usize;
            while let Some(entry) =
                entries.next_entry().await.map_err(|error| failure(&path, error))?
            {
                total += 1;
                if lines.len() >= MAX_ENTRIES {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                let kind = match entry.file_type().await {
                    Ok(file_type) if file_type.is_dir() => "dir ",
                    Ok(file_type) if file_type.is_symlink() => "link",
                    _ => "file",
                };
                lines.push(format!("{kind}  {name}"));
            }
            if total > MAX_ENTRIES {
                lines.push(format!("[showing {MAX_ENTRIES} of {total} entries]"));
            }
            if lines.is_empty() {
                return Ok(Value::String(format!("{path} is empty")));
            }
            Ok(Value::String(format!("{path}\n{}", lines.join("\n"))))
        })
    }
}

/// 取必填字符串参数。
fn required_string(arguments: &Map<String, Value>, name: &str) -> Result<String, ToolError> {
    arguments.get(name).and_then(Value::as_str).map(str::to_owned).ok_or_else(|| {
        ToolError::InvalidArguments {
            name: "file".to_owned(),
            reason: format!("missing required argument '{name}'"),
        }
    })
}

/// 取可选整数参数。
fn integer(arguments: &Map<String, Value>, name: &str) -> Option<i64> {
    arguments.get(name).and_then(Value::as_i64)
}

/// 构造文件访问失败的错误。
fn failure(path: &str, error: std::io::Error) -> ToolError {
    ToolError::ExecutionFailed {
        name: "file".to_owned(),
        reason: format!("{path}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    fn workspace(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("haloeth-fs-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("workspace should be created");
        dir
    }

    #[tokio::test]
    async fn writes_and_reads_a_file() {
        let dir = workspace("round-trip");
        let file = dir.join("a.txt");
        let tools = crate::tools::builtin_tools(&crate::tools::BuiltinOptions::default());

        tools
            .call(
                "write_file",
                &json!({ "path": file.to_string_lossy(), "content": "one\ntwo\nthree" }),
            )
            .await
            .expect("write should succeed");
        let output = tools
            .call("read_file", &json!({ "path": file.to_string_lossy() }))
            .await
            .expect("read should succeed");

        let text = output.as_str().expect("output should be text");
        assert!(text.contains("lines 1-3 of 3"));
        assert!(text.contains("one\ntwo\nthree"));
    }

    #[tokio::test]
    async fn read_file_pages_with_offset_and_limit() {
        let dir = workspace("paging");
        let file = dir.join("a.txt");
        std::fs::write(&file, "a\nb\nc\nd\ne").expect("fixture should be written");
        let tools = crate::tools::builtin_tools(&crate::tools::BuiltinOptions::default());

        let output = tools
            .call("read_file", &json!({ "path": file.to_string_lossy(), "offset": 2, "limit": 2 }))
            .await
            .expect("read should succeed");

        let text = output.as_str().expect("output should be text");
        assert!(text.contains("lines 2-3 of 5"));
        assert!(text.contains("b\nc"));
        assert!(!text.contains("\nd\n"));
    }

    #[tokio::test]
    async fn read_file_rejects_binary_content() {
        let dir = workspace("binary");
        let file = dir.join("blob.bin");
        std::fs::write(&file, [0_u8, 159, 146, 150, 0]).expect("fixture should be written");
        let tools = crate::tools::builtin_tools(&crate::tools::BuiltinOptions::default());

        let error = tools
            .call("read_file", &json!({ "path": file.to_string_lossy() }))
            .await
            .expect_err("binary files should be rejected");

        assert!(error.to_string().contains("binary"));
    }

    #[tokio::test]
    async fn write_file_requires_an_existing_parent() {
        let dir = workspace("missing-parent");
        let file = dir.join("missing").join("a.txt");
        let tools = crate::tools::builtin_tools(&crate::tools::BuiltinOptions::default());

        let error = tools
            .call("write_file", &json!({ "path": file.to_string_lossy(), "content": "x" }))
            .await
            .expect_err("missing parent should be reported");

        assert!(error.to_string().contains("parent directory"));
    }

    #[tokio::test]
    async fn list_dir_marks_entries_by_kind() {
        let dir = workspace("list");
        std::fs::write(dir.join("a.txt"), "x").expect("fixture should be written");
        std::fs::create_dir(dir.join("sub")).expect("fixture directory should be created");
        let tools = crate::tools::builtin_tools(&crate::tools::BuiltinOptions::default());

        let output =
            tools.call("list_dir", &json!({ "path": dir.to_string_lossy() })).await.expect("list");
        let text = output.as_str().expect("output should be text");

        assert!(text.contains("file  a.txt"));
        assert!(text.contains("dir   sub"));
    }
}
