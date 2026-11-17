//! 内置参考 Web 客户端。
//!
//! 单页应用直接按协议规范实现，用于验证「任意客户端」目标；
//! 样式与脚本全部内联，无需额外静态目录。

/// 单页应用内容。
pub const INDEX_HTML: &str = include_str!("../web/index.html");
