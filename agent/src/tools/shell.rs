//! Shell 工具：在平台原生 shell 中执行一条命令。
//!
//! 安全边界由审核层负责；工具自身只做尽力而为的约束：超时、输出上限、移除应用密钥环境变量，
//! 进程随 future 丢弃而终止。

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::error::ToolError;
use crate::text::truncate;
use crate::tool::{InputSpec, InputType, Tool, ToolEffect, ToolFuture, ToolSpec};

/// 默认执行超时。
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// 输出保留的最大字符数。
const MAX_OUTPUT: usize = 100_000;

/// 需要从子进程环境中移除的应用密钥变量。
const SECRET_ENV: [&str; 2] = ["HALOETH_API_KEY", "HALOETH_BASE_URL"];

/// 执行 shell 命令。
pub struct ShellTool {
    spec: ToolSpec,
    timeout: Duration,
}

impl ShellTool {
    /// 以默认超时构造工具。
    pub fn new() -> Self {
        let spec = ToolSpec::new(
            "shell",
            "Runs a shell command and returns its combined output and exit status. \
             Use it for tasks the other tools cannot express.",
            InputType::String,
        )
        .input("command", InputSpec::new(InputType::String, "Command line to run."))
        .input(
            "cwd",
            InputSpec::new(InputType::String, "Working directory; defaults to the app directory.")
                .nullable(),
        )
        .effect(ToolEffect::Command {
            argument: "command".to_owned(),
        });
        Self {
            spec,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// 设置执行超时。
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for ShellTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke<'a>(&'a self, arguments: &'a Map<String, Value>) -> ToolFuture<'a> {
        Box::pin(async move {
            let command = arguments.get("command").and_then(Value::as_str).unwrap_or_default();
            let cwd = arguments.get("cwd").and_then(Value::as_str);

            let mut process = platform_command(command);
            process.kill_on_drop(true).stdout(Stdio::piped()).stderr(Stdio::piped());
            for key in SECRET_ENV {
                process.env_remove(key);
            }
            if let Some(cwd) = cwd
                && !cwd.trim().is_empty()
            {
                process.current_dir(cwd);
            }

            let mut child = process.spawn().map_err(|error| ToolError::ExecutionFailed {
                name: "shell".to_owned(),
                reason: format!("failed to start the command: {error}"),
            })?;
            let pid = child.id();
            let mut stdout_pipe = child.stdout.take();
            let mut stderr_pipe = child.stderr.take();

            let collect = async {
                let (stdout, stderr) =
                    tokio::join!(read_to_end(&mut stdout_pipe), read_to_end(&mut stderr_pipe));
                let status = child.wait().await?;
                Ok::<_, std::io::Error>((stdout, stderr, status))
            };

            let (stdout, stderr, status) = match tokio::time::timeout(self.timeout, collect).await {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => {
                    return Err(ToolError::ExecutionFailed {
                        name: "shell".to_owned(),
                        reason: format!("failed to run the command: {error}"),
                    });
                }
                Err(_) => {
                    // 只杀直接子进程会留下孙进程，这里连同进程树一起终止。
                    if let Some(pid) = pid {
                        kill_tree(pid);
                    }
                    let _ = child.start_kill();
                    return Err(ToolError::ExecutionFailed {
                        name: "shell".to_owned(),
                        reason: format!("timed out after {} seconds", self.timeout.as_secs()),
                    });
                }
            };

            let stdout = String::from_utf8_lossy(&stdout);
            let stderr = String::from_utf8_lossy(&stderr);
            let mut text = format!("exit code: {}", status.code().unwrap_or(-1));
            if !stdout.trim().is_empty() {
                text.push_str("\nstdout:\n");
                text.push_str(stdout.trim_end());
            }
            if !stderr.trim().is_empty() {
                text.push_str("\nstderr:\n");
                text.push_str(stderr.trim_end());
            }
            Ok(Value::String(truncate(&text, MAX_OUTPUT)).into())
        })
    }
}

/// 读取管道到结束；管道被取走时返回空内容。
async fn read_to_end<R: tokio::io::AsyncRead + Unpin>(reader: &mut Option<R>) -> Vec<u8> {
    use tokio::io::AsyncReadExt as _;

    let mut buffer = Vec::new();
    if let Some(reader) = reader {
        let _ = reader.read_to_end(&mut buffer).await;
    }
    buffer
}

/// 终止整个进程树。
#[cfg(windows)]
fn kill_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 终止整个进程组。
#[cfg(not(windows))]
fn kill_tree(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-TERM", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 构造平台原生命令行。
#[cfg(windows)]
fn platform_command(command: &str) -> tokio::process::Command {
    let mut process = tokio::process::Command::new("cmd");
    process.arg("/C").arg(command);
    process
}

/// 构造平台原生命令行。
#[cfg(not(windows))]
fn platform_command(command: &str) -> tokio::process::Command {
    let mut process = tokio::process::Command::new("sh");
    process.arg("-c").arg(command);
    process
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> ShellTool {
        ShellTool::new()
    }

    #[tokio::test]
    async fn reports_output_and_exit_code() {
        let command = "echo hello";
        let output = tool()
            .invoke(&Map::from_iter([("command".to_owned(), Value::String(command.into()))]))
            .await
            .expect("command should run");

        let text = output.as_str().expect("output should be text");
        assert!(text.starts_with("exit code: 0"));
        assert!(text.contains("hello"));
    }

    #[tokio::test]
    async fn non_zero_exits_are_reported_in_the_observation() {
        let command = if cfg!(windows) {
            "exit /b 3"
        } else {
            "exit 3"
        };
        let output = tool()
            .invoke(&Map::from_iter([("command".to_owned(), Value::String(command.into()))]))
            .await
            .expect("command should run");

        assert!(output.as_str().expect("output should be text").contains("exit code: 3"));
    }

    #[tokio::test]
    async fn timeouts_are_reported_as_failures() {
        let command = if cfg!(windows) {
            "ping -n 10 127.0.0.1"
        } else {
            "sleep 10"
        };
        let mut arguments = Map::new();
        arguments.insert("command".to_owned(), Value::String(command.into()));
        let error = ShellTool::new()
            .with_timeout(Duration::from_millis(50))
            .invoke(&arguments)
            .await
            .expect_err("timeout should fail");

        assert!(error.to_string().contains("timed out"));
    }
}
