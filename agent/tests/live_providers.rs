//! 真实提供商的兼容性冒烟测试。
//!
//! 只有设置了 `HALOETH_TEST_KEYS_FILE`（指向包含 `名称：密钥` 行的文本文件）时才运行：
//!
//! ```sh
//! HALOETH_TEST_KEYS_FILE=path/to/keys.txt cargo test -p agent --test live_providers -- --ignored
//! ```
//!
//! 测试只发一条很短的对话与一次工具调用，避免无谓消耗。

use std::collections::BTreeMap;
use std::path::PathBuf;

use agent::message::{ContentBlock, Message, Role, StreamDelta};
use agent::model::{GenerateRequest, Modality, Model, ToolChoice};
use agent::openai::{HttpModelOptions, ToolChoicePolicy, build_model};
use agent::tool::{FunctionTool, InputSpec, InputType, Tool, ToolSet, ToolSpec, wire::WireTool};
use agent::tools::{SearchOptions, WebSearchTool};

/// 一个待测试的提供商。
struct Case {
    /// 密钥文件中的名称
    name: &'static str,
    /// 接口地址
    base_url: &'static str,
    /// 模型
    model: &'static str,
}

const CASES: &[Case] = &[
    Case {
        name: "deepseek",
        base_url: "https://api.deepseek.com",
        model: "deepseek-flash",
    },
    Case {
        name: "阿里云千问平台通用",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        model: "qwen3.8-flash",
    },
    Case {
        name: "智普通用",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        model: "glm-5.3-flash",
    },
];

#[tokio::test]
#[ignore = "requires HALOETH_TEST_KEYS_FILE"]
async fn providers_complete_a_tool_loop() {
    let Some(keys) = keys() else {
        return;
    };
    for case in CASES {
        let Some(key) = keys.get(case.name) else {
            continue;
        };
        let mut agent = agent_for(case, key);
        let result = agent
            .run("Call echo with text 'hi', then call final_answer with its result.", |_| {})
            .await
            .unwrap_or_else(|error| panic!("{} failed: {error}", case.name));
        println!("{} result: {:?}", case.name, result.output);
        assert!(
            result.output.to_string().contains("hi"),
            "{} did not complete the loop: {:?}",
            case.name,
            result.output
        );
    }
}

#[tokio::test]
#[ignore = "requires HALOETH_TEST_KEYS_FILE"]
async fn deepseek_search_returns_results() {
    let Some(keys) = keys() else {
        return;
    };
    let Some(key) = keys.get("deepseek") else {
        return;
    };
    let tool = WebSearchTool::new(SearchOptions::new("https://api.deepseek.com/tools/search", key))
        .expect("search tool");
    let mut arguments = serde_json::Map::new();
    arguments.insert("query".into(), serde_json::json!("rust programming language"));
    let output = tool.invoke(&arguments).await.expect("search");
    println!("search: {}", output.observation());
    assert!(output.observation().contains("http"));
}

#[tokio::test]
#[ignore = "requires HALOETH_TEST_KEYS_FILE"]
async fn bailian_accepts_image_input() {
    let Some(keys) = keys() else {
        return;
    };
    let Some(key) = keys.get("阿里云千问平台通用") else {
        return;
    };
    const BLUE_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAN0lEQVR4nO3RMQ0AMAzEQLf8OacQ3CWbD8BHcg4Mm+7qOh340A9UiVSJVIlUiVSJVIlUiVSJMA+hxgE/8m7SpQAAAABJRU5ErkJggg==";
    let model = build_model(
        HttpModelOptions::new("qwen3-vl-plus", key)
            .base_url("https://dashscope.aliyuncs.com/compatible-mode/v1")
            .tool_choice(ToolChoicePolicy::Auto)
            .input_modalities(vec![Modality::Text, Modality::Image]),
    );
    let request = GenerateRequest::new(vec![Message::new(
        Role::User,
        vec![
            ContentBlock::text("What is the color of this image? Answer with one word."),
            ContentBlock::image_base64("image/png", BLUE_PNG),
        ],
    )])
    .tool_choice(ToolChoice::None);
    let mut text = String::new();
    let message = model
        .generate(request, &mut |delta| {
            if let Some(content) = delta.content {
                text.push_str(&content);
            }
        })
        .await
        .expect("vision request");
    println!("vision: {text}");
    assert!(!message.content.is_empty());
    assert!(text.to_lowercase().contains("blue"), "unexpected answer: {text}");
}

/// 读取密钥文件。
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

/// 构造一个带 echo 工具的代理。
fn agent_for(case: &Case, key: &str) -> agent::Agent<Box<dyn Model>> {
    let mut tools = ToolSet::new();
    tools
        .insert(FunctionTool::new(
            ToolSpec::new("echo", "Echoes the given text.", InputType::String)
                .input("text", InputSpec::new(InputType::String, "Text to echo.")),
            |arguments: &serde_json::Map<String, serde_json::Value>| {
                Ok(arguments.get("text").cloned().unwrap_or_default())
            },
        ))
        .expect("echo tool");
    agent::Agent::new(model(case, key), tools).expect("agent")
}

/// 构造模型。
fn model(case: &Case, key: &str) -> Box<dyn Model> {
    build_model(
        HttpModelOptions::new(case.model, key)
            .base_url(case.base_url)
            .tool_choice(ToolChoicePolicy::Auto),
    )
}

#[tokio::test]
#[ignore = "requires HALOETH_TEST_KEYS_FILE"]
async fn providers_stream_a_short_answer() {
    let Some(keys) = keys() else {
        return;
    };
    for case in CASES {
        let Some(key) = keys.get(case.name) else {
            continue;
        };
        let model = model(case, key);
        let request = GenerateRequest::new(vec![
            Message::system("Answer with one short sentence."),
            Message::user("Say hello."),
        ])
        .tool_choice(ToolChoice::None);
        let mut text = String::new();
        let message = model
            .generate(request, &mut |delta: StreamDelta| {
                if let Some(content) = delta.content {
                    text.push_str(&content);
                }
            })
            .await
            .unwrap_or_else(|error| panic!("{} failed: {error}", case.name));
        println!("{}: {:?}", case.name, text);
        assert!(!message.content.is_empty(), "{} returned no content", case.name);
    }
}

#[tokio::test]
#[ignore = "requires HALOETH_TEST_KEYS_FILE"]
async fn providers_can_call_a_tool() {
    let Some(keys) = keys() else {
        return;
    };
    let tool = WireTool::from_spec(
        &ToolSpec::new("echo", "Echoes the given text.", InputType::String)
            .input("text", InputSpec::new(InputType::String, "Text to echo.")),
    );
    for case in CASES {
        let Some(key) = keys.get(case.name) else {
            continue;
        };
        let model = model(case, key);
        let request = GenerateRequest::new(vec![
            Message::system("You solve tasks by calling tools."),
            Message::user("Call the echo tool with text 'hi', then stop."),
        ])
        .tools(vec![tool.clone()])
        .tool_choice(ToolChoice::Auto);
        let message = model
            .generate(request, &mut |_| {})
            .await
            .unwrap_or_else(|error| panic!("{} failed: {error}", case.name));
        println!("{} tool calls: {:?}", case.name, message.tool_calls);
        assert!(!message.tool_calls.is_empty(), "{} did not call the tool", case.name);
        assert_eq!(message.tool_calls[0].name, "echo");
    }
}
