//! 提示词模板：系统提示、计划指令与兜底答案指令。
//!
//! 全部文本面向模型，使用英文，并由本模块作为文本的单点来源。模板变量以函数参数传入，
//! 不使用通用模板引擎。

use crate::approval::DirectoryGrant;
use crate::tool::{Access, ToolSet};

/// 渲染文件权限段落：说明用户当前授权的目录，以及没有固定工作区时应如何决策。
pub fn workspace_section(grants: &[DirectoryGrant]) -> String {
    if grants.is_empty() {
        return String::from(
            "\n\n## File permissions\n\n\
             The user has not granted any directory for this session, and there is no fixed workspace. \
             Decide from the user's request what is appropriate: prefer answering questions without \
             touching files, read only when the task needs it, and ask before writing anything. \
             File, shell and network operations may require the user's approval. Use absolute paths \
             unless the user names a directory.",
        );
    }

    let mut text = String::from(
        "\n\n## File permissions\n\nThe user granted access to these directories for this session:\n",
    );
    for grant in grants {
        let access = match grant.access {
            Access::Read => "read only",
            Access::Write => "read and write",
        };
        text.push_str(&format!("- {} ({access})\n", grant.path.display()));
    }
    text.push_str(
        "\nPrefer these directories when a task needs files, but the user's request takes precedence: \
         if they ask for another location, follow it and let the approval policy handle access. \
         Relative paths resolve against the first authorized directory. Operations outside these \
         directories are denied or require approval.",
    );
    text
}

/// 计划生成的停止标记。
pub const PLAN_STOP_SEQUENCE: &str = "<end_plan>";

/// 渲染系统提示（不含 Skill 与工作区）。
pub fn system_prompt(tools: &ToolSet, instructions: Option<&str>) -> String {
    system_prompt_with_context(tools, instructions, &[], &[])
}

/// 渲染系统提示：循环协议、示例、工具清单、Skill 清单、工作区、规则与附加指令。
pub fn system_prompt_with_skills(
    tools: &ToolSet,
    instructions: Option<&str>,
    skills: &[crate::skill::Skill],
) -> String {
    system_prompt_with_context(tools, instructions, skills, &[])
}

/// 渲染系统提示，并告知模型当前会话授权的目录。
///
/// 工作区是会话级授权，不是必填项：没有授权时明确告诉模型「没有工作区」，
/// 避免它默认某个目录可用。
pub fn system_prompt_with_context(
    tools: &ToolSet,
    instructions: Option<&str>,
    skills: &[crate::skill::Skill],
    grants: &[DirectoryGrant],
) -> String {
    let mut prompt = String::from(
        "You are an expert assistant who solves tasks by calling tools. You will be given a task to solve as best you can.\n\
         To do so, you have been given access to a set of tools.\n\
         \n\
         In every step you must call at least one tool: your tool call is an action, and after the tools run you receive their results as observations.\n\
         This action/observation cycle repeats until you call the final_answer tool, which is the only way to end the task.\n\
         \n\
         Here is an example using notional tools:\n\
         \n\
         Task: \"What is the result of 5 + 3?\"\n\
         Action: {\"name\": \"python_interpreter\", \"arguments\": {\"code\": \"5 + 3\"}}\n\
         Observation: 8\n\
         Action: {\"name\": \"final_answer\", \"arguments\": {\"answer\": 8}}\n\
         \n\
         Above example used a notional tool that might not exist for you. You only have access to these tools:\n",
    );
    prompt.push_str(&tools.describe());
    if !skills.is_empty() {
        prompt.push_str(
            "\n\nWhen a task matches one of these skills, call load_skill with its name to read the full instructions:\n",
        );
        for skill in skills {
            prompt.push_str(&skill.summary());
            prompt.push('\n');
        }
    }
    prompt.push_str(&workspace_section(grants));
    prompt.push_str(
        "\n\nHere are the rules you should always follow to solve your task:\n\
         1. Always provide at least one tool call in every step; a reply without any tool call ends the run and is shown to the user as your final answer.\n\
         2. Always use the right arguments for the tools: use the exact argument names declared by each tool, and pass the value itself, never a variable name.\n\
         3. Call a tool only when needed, and never re-do a tool call that you previously did with the exact same parameters.\n\
         4. Read the observations carefully before deciding the next action.\n\
         5. When you have the answer, call the final_answer tool and make it the only tool call of its step.\n\
         6. Don't give up: you are in charge of solving the task, not of providing directions to solve it.\n",
    );
    if let Some(instructions) = instructions {
        prompt.push('\n');
        prompt.push_str(instructions);
        prompt.push('\n');
    }
    prompt.push_str("\nNow begin!");
    prompt
}

/// 渲染初始计划指令。
pub fn initial_plan(task: &str, tools: &ToolSet) -> String {
    format!(
        "You are a world expert at analyzing a situation to derive facts, and plan accordingly towards solving a task.\n\
         Below I will present you a task. You will need to 1. build a survey of facts known or needed to solve the task, then 2. make a plan of action to solve the task.\n\
         \n\
         ## 1. Facts survey\n\
         ### 1.1. Facts given in the task\n\
         List here the specific facts given in the task that could help you (there might be nothing here).\n\
         ### 1.2. Facts to look up\n\
         List here any facts that we may need to look up.\n\
         ### 1.3. Facts to derive\n\
         List here anything that we want to derive from the above by logical reasoning.\n\
         \n\
         Don't make any assumptions. For each item, provide a thorough reasoning. Do not add anything else on top of the three headings above.\n\
         \n\
         ## 2. Plan\n\
         Then for the given task, develop a step-by-step high-level plan taking into account the above inputs and list of facts.\n\
         This plan should involve individual tasks based on the available tools, that if executed correctly will yield the correct answer.\n\
         Do not skip steps, do not add any superfluous steps. Only write the high-level plan, DO NOT DETAIL INDIVIDUAL TOOL CALLS.\n\
         After writing the final step of the plan, write the '{PLAN_STOP_SEQUENCE}' tag and stop there.\n\
         \n\
         You can leverage these tools:\n\
         {tools}\n\
         \n\
         ---\n\
         Now begin! Here is your task:\n\
         ```\n\
         {task}\n\
         ```\n\
         First in part 1, write the facts survey, then in part 2, write your plan.",
        tools = tools.describe(),
        task = task,
    )
}

/// 渲染计划更新的前置消息，作为系统消息使用。
pub fn plan_update_preamble(task: &str) -> String {
    format!(
        "You are a world expert at analyzing a situation, and plan accordingly towards solving a task.\n\
         You have been given the following task:\n\
         ```\n\
         {task}\n\
         ```\n\
         \n\
         Below you will find a history of attempts made to solve this task.\n\
         You will first have to produce a survey of known and unknown facts, then propose a step-by-step high-level plan to solve the task.\n\
         If the previous tries so far have met some success, your updated plan can build on these results.\n\
         If you are stalled, you can make a completely new plan starting from scratch.\n\
         \n\
         Find the task and history below:"
    )
}

/// 渲染计划更新的后置消息，作为用户消息使用。
pub fn plan_update_request(tools: &ToolSet, remaining_steps: u32) -> String {
    format!(
        "Now write your updated facts below, taking into account the above history:\n\
         ## 1. Updated facts survey\n\
         ### 1.1. Facts given in the task\n\
         ### 1.2. Facts that we have learned\n\
         ### 1.3. Facts still to look up\n\
         ### 1.4. Facts still to derive\n\
         \n\
         Then write a step-by-step high-level plan to solve the task above.\n\
         ## 2. Plan\n\
         This plan should involve individual tasks based on the available tools, that if executed correctly will yield the correct answer.\n\
         Beware that you have {remaining_steps} steps remaining.\n\
         Do not skip steps, do not add any superfluous steps. Only write the high-level plan, DO NOT DETAIL INDIVIDUAL TOOL CALLS.\n\
         After writing the final step of the plan, write the '{PLAN_STOP_SEQUENCE}' tag and stop there.\n\
         \n\
         You can leverage these tools:\n\
         {tools}\n\
         \n\
         Now write your new plan below.",
        tools = tools.describe(),
    )
}

/// 渲染兜底答案的前置消息，作为系统消息使用。
pub fn final_answer_preamble() -> String {
    String::from(
        "An agent tried to answer a user query but it got stuck and failed to do so. You are tasked with providing an answer instead. Here is the agent's memory:",
    )
}

/// 渲染兜底答案的后置消息，作为用户消息使用。
pub fn final_answer_request(task: &str) -> String {
    format!("Based on the above, please provide an answer to the following user task:\n{task}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{FunctionTool, InputSpec, InputType, ToolSpec};
    use serde_json::{Map, Value};

    fn tool_set() -> ToolSet {
        let mut tools = ToolSet::new();
        tools
            .insert(FunctionTool::new(
                ToolSpec::new("echo", "Echoes the given text.", InputType::String)
                    .input("text", InputSpec::new(InputType::String, "Text to echo.")),
                |arguments: &Map<String, Value>| {
                    Ok(arguments.get("text").cloned().unwrap_or(Value::Null))
                },
            ))
            .expect("insertion should succeed");
        tools
    }

    #[test]
    fn system_prompt_lists_tools_and_rules() {
        let prompt = system_prompt(&tool_set(), None);

        assert!(prompt.contains("echo: Echoes the given text."));
        assert!(prompt.contains("- text (string): Text to echo."));
        assert!(prompt.contains("Always provide at least one tool call"));
        assert!(prompt.contains("make it the only tool call of its step"));
        assert!(prompt.ends_with("Now begin!"));
    }

    #[test]
    fn system_prompt_lists_enabled_skills() {
        let skill = crate::skill::Skill {
            name: "pdf".into(),
            description: "Work with PDFs.".into(),
            when_to_use: "the user mentions a PDF".into(),
            body: "Body.".into(),
            root: std::path::PathBuf::from("."),
        };

        let prompt = system_prompt_with_skills(&tool_set(), None, &[skill]);

        assert!(prompt.contains("call load_skill"));
        assert!(prompt.contains("- pdf: Work with PDFs."));
        assert!(prompt.contains("use when: the user mentions a PDF"));
    }

    #[test]
    fn system_prompt_appends_instructions() {
        let prompt = system_prompt(&tool_set(), Some("Always answer in Chinese."));

        assert!(prompt.contains("\nAlways answer in Chinese.\n"));
    }

    #[test]
    fn workspace_section_lists_grants_and_reports_an_empty_workspace() {
        let grants = vec![
            DirectoryGrant::new(std::path::PathBuf::from("/work"), Access::Write),
            DirectoryGrant::new(std::path::PathBuf::from("/docs"), Access::Read),
        ];

        let prompt = system_prompt_with_context(&tool_set(), None, &[], &grants);

        assert!(prompt.contains("## File permissions"));
        assert!(prompt.contains("- /work (read and write)"));
        assert!(prompt.contains("- /docs (read only)"));
        assert!(prompt.contains("the user's request takes precedence"));

        let empty = system_prompt_with_context(&tool_set(), None, &[], &[]);
        assert!(empty.contains("there is no fixed workspace"));
        assert!(empty.contains("prefer answering questions without"));
    }

    #[test]
    fn initial_plan_embeds_task_tools_and_stop_marker() {
        let prompt = initial_plan("Who wrote Hamlet?", &tool_set());

        assert!(prompt.contains("Who wrote Hamlet?"));
        assert!(prompt.contains("echo: Echoes the given text."));
        assert!(prompt.contains(PLAN_STOP_SEQUENCE));
    }

    #[test]
    fn plan_update_prompts_embed_task_and_remaining_steps() {
        let preamble = plan_update_preamble("Who wrote Hamlet?");
        let request = plan_update_request(&tool_set(), 7);

        assert!(preamble.contains("Who wrote Hamlet?"));
        assert!(preamble.contains("history of attempts"));
        assert!(request.contains("you have 7 steps remaining"));
        assert!(request.contains("echo: Echoes the given text."));
    }

    #[test]
    fn final_answer_prompts_embed_task() {
        let preamble = final_answer_preamble();
        let request = final_answer_request("Who wrote Hamlet?");

        assert!(preamble.contains("got stuck and failed"));
        assert!(request.contains("Who wrote Hamlet?"));
    }
}
