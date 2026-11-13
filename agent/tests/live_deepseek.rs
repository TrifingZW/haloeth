//! 真实提供商联调：验证完整的工具循环。
//!
//! 设置 `DEEPSEEK_API_KEY` 后运行：
//! `cargo test -p agent --test live_deepseek -- --ignored`

use agent::openai::{HttpModel, HttpModelOptions};
use agent::tools::{BuiltinOptions, builtin_tools};
use agent::{Agent, RunState};

#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY"]
async fn deepseek_completes_a_tool_loop() {
    let Ok(key) = std::env::var("DEEPSEEK_API_KEY") else {
        panic!("set DEEPSEEK_API_KEY to run this test");
    };
    let model = HttpModel::new(
        HttpModelOptions::new("deepseek-flash", key).base_url("https://api.deepseek.com"),
    );
    let mut agent = Agent::new(model, builtin_tools(&BuiltinOptions::default()))
        .expect("agent should be constructed")
        .with_max_steps(4);

    let result = agent
        .run("Call the current_time tool, then call final_answer with the word ok.", |_| {})
        .await
        .expect("run should succeed");

    assert_eq!(result.state, RunState::Success);
    assert!(!agent.memory().steps.is_empty());
}
