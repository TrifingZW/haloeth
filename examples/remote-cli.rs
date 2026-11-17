//! 参考命令行客户端：连接远程服务并收发消息。
//!
//! 用法：`cargo run --example remote-cli -- <ws-url 或 wss-url> <token> [--fp <指纹>]`
//!
//! 地址、令牌与指纹来自偏好设置「远程」中的配对二维码。

use std::io::{BufRead, Write};

use remote::proto::dto::ChatRoleDto;
use serde_json::json;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let (Some(url), Some(token)) = (arguments.next(), arguments.next()) else {
        usage();
        std::process::exit(2);
    };
    let mut fingerprint = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--fp" => fingerprint = arguments.next(),
            other => {
                eprintln!("未知参数：{other}");
                usage();
                std::process::exit(2);
            }
        }
    }

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("创建运行时失败：{error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = runtime.block_on(run(url, token, fingerprint)) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

/// 打印用法。
fn usage() {
    eprintln!("用法：remote-cli <ws-url 或 wss-url> <token> [--fp <证书指纹>]");
}

/// 命令行主循环：标准输入与通知流并行处理。
async fn run(
    url: String,
    token: String,
    fingerprint: Option<String>,
) -> Result<(), remote::client::ClientError> {
    let mut client = match fingerprint.as_deref() {
        Some(fingerprint) => {
            remote::client::Client::connect_pinned(&url, &token, fingerprint).await?
        }
        None => remote::client::Client::connect(&url, &token).await?,
    };
    println!(
        "已连接：{url}（服务端 {}）",
        client.welcome()["server"]["version"].as_str().unwrap_or("-")
    );
    println!("输入 /help 查看命令；其它输入作为消息发送。");

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else {
                break;
            };
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    loop {
        let line = tokio::select! {
            notification = client.next_notification() => {
                match notification {
                    Ok(Some((_seq, notification))) => print_notification(&notification),
                    Ok(None) => {
                        println!("连接已关闭");
                        break;
                    }
                    Err(error) => {
                        eprintln!("{error}");
                        break;
                    }
                }
                continue;
            }
            line = receiver.recv() => line,
        };
        let Some(line) = line else {
            break;
        };
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        if let Some(id) = line.strip_prefix("/approve ") {
            resolve(&mut client, id.trim(), "allow_once").await;
            continue;
        }
        if let Some(id) = line.strip_prefix("/deny ") {
            resolve(&mut client, id.trim(), "deny").await;
            continue;
        }
        match line.as_str() {
            "/quit" | "/exit" => break,
            "/help" => print_help(),
            "/cancel" => match client.request("run.cancel", json!({})).await {
                Ok(_) => println!("已请求取消"),
                Err(error) => println!("取消失败：{error}"),
            },
            "/state" => match client.request("state.get", json!({})).await {
                Ok(value) => println!(
                    "{} · {} · {}",
                    value["state"]["title"].as_str().unwrap_or("-"),
                    value["state"]["status"].as_str().unwrap_or("-"),
                    value["state"]["activity"].as_str().unwrap_or("")
                ),
                Err(error) => println!("读取失败：{error}"),
            },
            _ => match client.request("message.send", json!({ "text": line })).await {
                Ok(_) => {}
                Err(error) if error.code == remote::ErrorCode::Busy => {
                    let text = error
                        .data
                        .as_ref()
                        .and_then(|data| data.get("text"))
                        .and_then(|text| text.as_str())
                        .unwrap_or_default();
                    println!("运行中，已拒绝：{text}");
                }
                Err(error) => println!("发送失败：{error}"),
            },
        }
    }
    Ok(())
}

/// 提交一次审批。
async fn resolve(client: &mut remote::client::Client, id: &str, decision: &str) {
    if id.is_empty() {
        println!("用法：/approve <id> 或 /deny <id>");
        return;
    }
    match client.request("approval.resolve", json!({ "id": id, "decision": decision })).await {
        Ok(_) => println!("已提交审批：{id} → {decision}"),
        Err(error) => println!("审批失败：{error}"),
    }
}

/// 打印命令帮助。
fn print_help() {
    println!("命令：");
    println!("  /approve <id> /deny <id>  处理审批");
    println!("  /cancel                  停止当前运行");
    println!("  /state                   查看当前状态");
    println!("  /quit                    退出");
    println!("其它输入作为消息发送。");
}

/// 打印一条服务端通知。
fn print_notification(notification: &remote::Notification) {
    use remote::Notification;
    match notification {
        Notification::TranscriptReset(reset) => {
            println!("\n[会话] {} 条记录", reset.entries.len())
        }
        Notification::MessageAppended(appended) => {
            println!("\n{}：{}", role_label(appended.role), appended.text)
        }
        Notification::MessageDelta(delta) => {
            print!("{}", delta.text);
            let _ = std::io::stdout().flush();
        }
        Notification::MessageCompleted(completed) => match &completed.text {
            Some(text) => println!("\n助手：{text}"),
            None => println!(),
        },
        Notification::ReasoningDelta(delta) => {
            print!("（思考）{}", delta.text);
            let _ = std::io::stdout().flush();
        }
        Notification::ToolStarted(started) => {
            println!("\n[工具] {} 开始：{}", started.name, started.arguments)
        }
        Notification::ToolFinished(finished) => println!(
            "[工具] {} {} · {} ms · {}",
            finished.call_id,
            if finished.ok {
                "完成"
            } else {
                "失败"
            },
            finished.elapsed_ms,
            finished.preview
        ),
        Notification::ApprovalRequested(requested) => println!(
            "\n[审批] {}（{}，风险 {:?}）：/approve {} 或 /deny {}",
            requested.summary, requested.tool, requested.risk, requested.id, requested.id
        ),
        Notification::ApprovalResolved(resolved) => println!(
            "[审批] {} → {:?}{}",
            resolved.id,
            resolved.decision,
            if resolved.reason.is_empty() {
                String::new()
            } else {
                format!(" · {}", resolved.reason)
            }
        ),
        Notification::ImagesAppended(images) => {
            for item in &images.items {
                println!("[图片] {}", item.path);
            }
        }
        Notification::PlanAppended(plan) => println!("\n[计划]\n{}", plan.text),
        Notification::StatusChanged(changed) => println!(
            "[状态] {:?}{}",
            changed.status,
            changed
                .activity
                .as_deref()
                .map(|activity| format!(" · {activity}"))
                .unwrap_or_default()
        ),
        Notification::UsageChanged(usage) => {
            println!("[用量] 输入 {} / 输出 {}", usage.total.input, usage.total.output)
        }
        Notification::SessionsChanged(sessions) => {
            println!("[会话列表] {} 个", sessions.summaries.len())
        }
        Notification::StateChanged(state) => {
            println!("[状态] {} · {:?}", state.state.title, state.state.status)
        }
        Notification::Shutdown(shutdown) => println!("[服务] 已停止：{}", shutdown.reason),
    }
}

/// 角色标签。
fn role_label(role: ChatRoleDto) -> &'static str {
    match role {
        ChatRoleDto::User => "你",
        ChatRoleDto::Assistant => "助手",
    }
}
