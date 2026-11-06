//! 文本工具函数。

/// 截断文本并追加省略标记。
pub fn truncate(text: &str, max_length: usize) -> String {
    if text.chars().count() <= max_length {
        return text.to_owned();
    }
    let head: String = text.chars().take(max_length).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_short_text_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncates_on_character_boundaries() {
        let text = "中".repeat(5);

        assert_eq!(truncate(&text, 3), "中中中…");
    }
}
