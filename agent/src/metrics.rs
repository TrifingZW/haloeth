//! 运行指标类型：**Token** 用量与计时。

use std::ops::Add;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// 一次生成的 **Token** 用量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// 输入 **Token** 数。
    pub input_tokens: u32,
    /// 输出 **Token** 数。
    pub output_tokens: u32,
}

impl TokenUsage {
    /// 构造用量。
    pub fn new(input_tokens: u32, output_tokens: u32) -> Self {
        Self {
            input_tokens,
            output_tokens,
        }
    }

    /// 合计 **Token** 数。
    pub fn total(self) -> u32 {
        self.input_tokens + self.output_tokens
    }

    /// 逐字段相减，避免下溢。
    pub fn saturating_sub(self, other: Self) -> Self {
        Self::new(
            self.input_tokens.saturating_sub(other.input_tokens),
            self.output_tokens.saturating_sub(other.output_tokens),
        )
    }
}

impl Add for TokenUsage {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self::new(self.input_tokens + other.input_tokens, self.output_tokens + other.output_tokens)
    }
}

/// 一段过程的时间信息。
///
/// 持久化时只保存耗时：`Instant` 没有跨进程的可移植表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    started: Instant,
    finished: Option<Instant>,
}

impl Timing {
    /// 开始计时。
    pub fn start() -> Self {
        Self {
            started: Instant::now(),
            finished: None,
        }
    }

    /// 以已知耗时构造一段已结束的计时，用于从持久化状态恢复。
    pub fn from_duration(duration: Duration) -> Self {
        let finished = Instant::now();
        Self {
            started: finished - duration,
            finished: Some(finished),
        }
    }

    /// 结束计时；重复调用不改变结果。
    pub fn finish(&mut self) {
        if self.finished.is_none() {
            self.finished = Some(Instant::now());
        }
    }

    /// 返回结束时刻。
    pub fn finished(self) -> Option<Instant> {
        self.finished
    }

    /// 返回耗时；未结束时返回 [`None`]。
    pub fn duration(self) -> Option<Duration> {
        self.finished.map(|finished| finished.saturating_duration_since(self.started))
    }

    /// 返回从开始到当前（或结束）的耗时。
    pub fn elapsed(self) -> Duration {
        self.finished.unwrap_or_else(Instant::now).saturating_duration_since(self.started)
    }
}

impl Serialize for Timing {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.elapsed().as_millis() as u64)
    }
}

impl<'de> Deserialize<'de> for Timing {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let millis = u64::deserialize(deserializer)?;
        Ok(Self::from_duration(Duration::from_millis(millis)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_is_the_sum_of_both_directions() {
        assert_eq!(TokenUsage::new(120, 30).total(), 150);
    }

    #[test]
    fn usage_adds_component_wise() {
        let total = TokenUsage::new(10, 5) + TokenUsage::new(3, 2);

        assert_eq!(total, TokenUsage::new(13, 7));
    }

    #[test]
    fn timing_reports_duration_only_after_finishing() {
        let mut timing = Timing::start();

        assert_eq!(timing.duration(), None);

        timing.finish();
        let duration = timing.duration().expect("duration should be recorded");

        assert_eq!(duration, timing.elapsed());
    }

    #[test]
    fn finishing_twice_keeps_the_first_result() {
        let mut timing = Timing::start();
        timing.finish();
        let first = timing.duration();
        timing.finish();

        assert_eq!(timing.duration(), first);
    }
}
