//! 真实生成服务的冒烟测试（图片）。
//!
//! 通过 `HALOETH_TEST_KEYS_FILE` 指向 `名称：密钥` 文本文件后运行：
//!
//! ```sh
//! HALOETH_TEST_KEYS_FILE=path/to/keys.txt cargo test -p agent --test live_media -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use agent::tool::{Tool, ToolOutput};
use agent::tools::{ImageOptions, ImageTool, JobOptions, JobTool};
use serde_json::json;

fn keys() -> Option<BTreeMap<String, String>> {
    let path = std::env::var_os("HALOETH_TEST_KEYS_FILE").map(PathBuf::from)?;
    let text = std::fs::read_to_string(path).ok()?;
    let mut keys = BTreeMap::new();
    for line in text.lines() {
        let Some((name, key)) = line.split_once('：') else {
            continue;
        };
        keys.insert(name.trim().to_owned(), key.trim().to_owned());
    }
    Some(keys)
}

fn output(name: &str) -> String {
    std::env::temp_dir().join(name).display().to_string()
}

#[tokio::test]
#[ignore = "requires HALOETH_TEST_KEYS_FILE"]
async fn zhipu_generates_an_image() {
    let Some(keys) = keys() else {
        return;
    };
    let Some(key) = keys.get("智普通用") else {
        return;
    };
    let tool = ImageTool::new(ImageOptions {
        endpoint: "https://open.bigmodel.cn/api/paas/v4/images/generations".into(),
        api_key: key.clone(),
        model: "cogview-3-flash".into(),
        headers: Default::default(),
    });
    let mut arguments = serde_json::Map::new();
    arguments.insert("prompt".into(), json!("a small blue dot on a white background"));
    arguments.insert("output_path".into(), json!(output("haloeth-zhipu.png")));
    let result = tool.invoke(&arguments).await.expect("image");
    println!("zhipu: {}", result.observation());
    assert!(matches!(result, ToolOutput::Blocks(_)));
}

#[tokio::test]
#[ignore = "requires HALOETH_TEST_KEYS_FILE"]
async fn dashscope_generates_an_image_asynchronously() {
    let Some(keys) = keys() else {
        return;
    };
    let Some(key) = keys.get("阿里云千问平台通用") else {
        return;
    };
    let mut headers = BTreeMap::new();
    headers.insert("X-DashScope-Async".to_owned(), "enable".to_owned());
    let tool = JobTool::new_image(
        "generate_image",
        "Generates an image.",
        JobOptions {
            name: "AI 绘图".into(),
            endpoint:
                "https://dashscope.aliyuncs.com/api/v1/services/aigc/text2image/image-synthesis"
                    .into(),
            api_key: key.clone(),
            model: "wanx2.1-t2i-turbo".into(),
            body_template: Some(json!({
                "model": "wanx2.1-t2i-turbo",
                "input": { "prompt": "{{prompt}}" },
                "parameters": { "size": "1024*1024", "n": 1 },
            })),
            poll_url: "https://dashscope.aliyuncs.com/api/v1/tasks/{{id}}".into(),
            id_path: "output.task_id".into(),
            status_path: "output.task_status".into(),
            done_value: "SUCCEEDED".into(),
            result_path: "output.results.0.url".into(),
            interval: std::time::Duration::from_secs(2),
            timeout: std::time::Duration::from_secs(180),
            headers,
        },
    );
    let mut arguments = serde_json::Map::new();
    arguments.insert("prompt".into(), json!("a small blue dot on a white background"));
    arguments.insert("output_path".into(), json!(output("haloeth-dashscope.png")));
    let result = tool.invoke(&arguments).await.expect("image");
    println!("dashscope: {}", result.observation());
    assert!(matches!(result, ToolOutput::Blocks(_)));
}
