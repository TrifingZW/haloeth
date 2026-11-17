//! Haloeth 桌面智能体：进程入口与模块声明。
//!
//! 主窗口与运行链路的装配在 `app`；界面组件在 `ui`，配置界面在 `settings`，
//! 会话运行与存储在 `session` 与 `session_store`。应用内所有边框统一 2px，
//! 组件自带的 1px 边框在调用处覆盖。

// Release 构建不附带控制台窗口；Debug 保留控制台，便于查看日志与调试输出。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod background;
mod config;
mod logging;
mod session;
mod session_store;
mod settings;
mod ui;

fn main() {
    app::run();
}
