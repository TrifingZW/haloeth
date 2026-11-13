//! 前端语音能力：识别、合成与 WAV 封装。
//!
//! 这些能力依赖窗口与系统 API，放在前端 crate；代理内核保持与语音无关。

pub mod microphone;
pub mod recognizer;
pub mod tts;
pub mod wav;

pub use microphone::{SelectableMicrophone, input_devices};
pub use recognizer::ConfiguredRecognizer;
pub use tts::{TtsService, TtsSettings};
