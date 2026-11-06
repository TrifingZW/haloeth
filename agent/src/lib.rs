//! Haloeth 的代理内核：消息表示、工具契约、模型抽象与编排循环。

pub mod agent;
pub mod default_tools;
pub mod error;
pub mod memory;
pub mod message;
pub mod metrics;
pub mod model;
pub mod openai;
pub mod prompt;
pub mod text;
pub mod tool;

pub use agent::{Agent, Event, Interrupt, RunResult, RunState};
