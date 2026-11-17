//! Haloeth 的代理内核：消息表示、工具契约、模型抽象与编排循环。

pub mod approval;
pub mod error;
pub mod mcp;
pub mod memory;
pub mod message;
pub mod metrics;
pub mod model;
pub mod runtime;
pub mod skill;
pub mod text;
pub mod tool;
pub mod tools;

pub use runtime::{Activity, Agent, Event, Interrupt, RunResult, RunState};
