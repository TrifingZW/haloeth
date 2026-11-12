//! 内置能力：文件读写、目录列举、Shell 与提供商搜索。
//!
//! 工具本身不限制路径：越界判断由审核层完成，这样同一套工具可以在不同会话的授权下复用。
//! 单个工具只负责执行与自我约束（大小上限、超时、二进制拒绝）。

pub mod fs;
pub mod search;
pub mod shell;

use crate::tool::ToolSet;

pub use fs::{ListDirTool, ReadFileTool, WriteFileTool};
pub use search::{SearchOptions, WebSearchTool};
pub use shell::ShellTool;

/// 内置工具集的装配选项。
#[derive(Debug, Clone, Default)]
pub struct BuiltinOptions {
    /// 搜索配置；为空时不装配 `web_search`。
    pub search: Option<SearchOptions>,
    /// 需要关闭的工具名称。
    pub disabled: Vec<String>,
}

/// 装配内置工具集：网页访问、当前时间、文件、Shell 与可选搜索。
pub fn builtin_tools(options: &BuiltinOptions) -> ToolSet {
    let mut tools = crate::default_tools::default_tools();
    tools.insert(ReadFileTool::new()).expect("内置工具名称固定且互不冲突");
    tools.insert(WriteFileTool::new()).expect("内置工具名称固定且互不冲突");
    tools.insert(ListDirTool::new()).expect("内置工具名称固定且互不冲突");
    tools.insert(ShellTool::new()).expect("内置工具名称固定且互不冲突");
    if let Some(search) = &options.search
        && let Ok(tool) = WebSearchTool::new(search.clone())
    {
        tools.insert(tool).expect("内置工具名称固定且互不冲突");
    }
    tools.retain(|name| !options.disabled.iter().any(|disabled| disabled == name));
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_tools_expose_the_documented_names() {
        let tools = builtin_tools(&BuiltinOptions::default());
        let names: Vec<&str> = tools.iter().map(|tool| tool.spec().name.as_str()).collect();

        assert_eq!(
            names,
            ["current_time", "fetch_url", "list_dir", "read_file", "shell", "write_file"]
        );
    }

    #[test]
    fn disabled_tools_are_removed() {
        let options = BuiltinOptions {
            disabled: vec!["fetch_url".to_owned(), "shell".to_owned()],
            ..Default::default()
        };
        let tools = builtin_tools(&options);

        assert!(tools.get("fetch_url").is_none());
        assert!(tools.get("shell").is_none());
        assert!(tools.get("read_file").is_some());
    }
}
