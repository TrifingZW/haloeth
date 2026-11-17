//! 内置能力：文件读写、目录列举、Shell 与提供商搜索。
//!
//! 工具本身不限制路径：越界判断由审核层完成，这样同一套工具可以在不同会话的授权下复用。
//! 单个工具只负责执行与自我约束（大小上限、超时、二进制拒绝）。

pub mod fs;
pub mod media;
pub mod search;
pub mod shell;

use crate::tool::ToolSet;

pub use fs::{ListDirTool, ReadFileTool, WriteFileTool};
pub use media::{ImageOptions, ImageTool, JobOptions, JobTool, ShowImageTool};
pub use search::{SearchOptions, WebSearchTool};
pub use shell::ShellTool;

/// 内置工具集的装配选项。
#[derive(Debug, Clone, Default)]
pub struct BuiltinOptions {
    /// 搜索配置；为空时不装配 `web_search`。
    pub search: Option<SearchOptions>,
    /// 需要关闭的工具名称。
    pub disabled: Vec<String>,
    /// 绘图配置（OpenAI 兼容同步接口）。
    pub image: Option<ImageOptions>,
    /// 绘图配置（异步任务接口）。
    pub image_job: Option<JobOptions>,
    /// 视频生成配置。
    pub video: Option<JobOptions>,
    /// 音乐生成配置。
    pub music: Option<JobOptions>,
}

/// 装配内置工具集：网页访问、当前时间、文件、Shell 与可选搜索。
pub fn builtin_tools(options: &BuiltinOptions) -> ToolSet {
    let mut tools = crate::default_tools::default_tools();
    tools.insert(ReadFileTool::new()).expect("内置工具名称固定且互不冲突");
    tools.insert(WriteFileTool::new()).expect("内置工具名称固定且互不冲突");
    tools.insert(ListDirTool::new()).expect("内置工具名称固定且互不冲突");
    tools.insert(ShellTool::new()).expect("内置工具名称固定且互不冲突");
    tools.insert(ShowImageTool::new()).expect("内置工具名称固定且互不冲突");
    if let Some(search) = &options.search
        && let Ok(tool) = WebSearchTool::new(search.clone())
    {
        tools.insert(tool).expect("内置工具名称固定且互不冲突");
    }
    if let Some(image) = &options.image {
        tools.insert(ImageTool::new(image.clone())).expect("内置工具名称固定且互不冲突");
    }
    if let Some(image) = &options.image_job {
        tools
            .insert(JobTool::new_image(
                media::GENERATE_IMAGE,
                "Generates an image from a text prompt and saves it to output_path.",
                image.clone(),
            ))
            .expect("内置工具名称固定且互不冲突");
    }
    if let Some(video) = &options.video {
        tools
            .insert(JobTool::new(
                media::GENERATE_VIDEO,
                "Generates a short video from a text prompt and saves it to output_path.",
                video.clone(),
            ))
            .expect("内置工具名称固定且互不冲突");
    }
    if let Some(music) = &options.music {
        tools
            .insert(JobTool::new(
                media::GENERATE_MUSIC,
                "Generates music from a text prompt and saves it to output_path.",
                music.clone(),
            ))
            .expect("内置工具名称固定且互不冲突");
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
            [
                "current_time",
                "fetch_url",
                "list_dir",
                "read_file",
                "shell",
                "show_image",
                "write_file"
            ]
        );
    }

    #[test]
    fn media_tools_are_registered_only_when_configured() {
        let options = BuiltinOptions {
            image: Some(crate::tools::ImageOptions {
                endpoint: "https://example.com/images".into(),
                api_key: String::new(),
                model: "img".into(),
                headers: Default::default(),
            }),
            ..Default::default()
        };
        let tools = builtin_tools(&options);
        assert!(tools.get("generate_image").is_some());
        assert!(tools.get("generate_video").is_none());

        let options = BuiltinOptions {
            image_job: Some(JobOptions {
                name: "image".into(),
                endpoint: "https://example.com/tasks".into(),
                api_key: String::new(),
                model: "img".into(),
                body_template: None,
                poll_url: "https://example.com/tasks/{{id}}".into(),
                id_path: "id".into(),
                status_path: "status".into(),
                done_value: "done".into(),
                result_path: "url".into(),
                interval: std::time::Duration::from_millis(200),
                timeout: std::time::Duration::from_secs(5),
                headers: Default::default(),
            }),
            ..Default::default()
        };
        let tools = builtin_tools(&options);
        assert!(tools.get("generate_image").is_some());
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
