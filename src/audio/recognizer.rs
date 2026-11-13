//! 语音识别：优先使用配置的云端模型，离线时可退回系统听写。
//!
//! 云端路径把麦克风收到的 16 kHz 单声道 PCM 累积起来，在用户停止录音后编码为
//! WAV，调用提供商的 `audio/transcriptions` 接口。网络请求在后台线程执行，
//! 结果通过 `SpeechSink` 回到界面线程。

use std::sync::Arc;

use gpui_kit::component::speech::{
    AudioFormat, RecognitionSession, SpeechError, SpeechRecognizer, SpeechSink, SystemRecognizer,
};
use gpui_kit::*;

use crate::config::AudioBackend;
use crate::session::Session;

/// 一次云端识别的配置快照。
#[derive(Clone)]
struct CloudConfig {
    endpoint: String,
    api_key: String,
    model: String,
    language: String,
    /// 百炼原生多模态接口使用 JSON 与 Base64 音频。
    dashscope: bool,
    /// 附加请求头。
    headers: std::collections::BTreeMap<String, String>,
    runtime: Option<Arc<tokio::runtime::Runtime>>,
}

/// 按偏好分发的识别器：云端模型或系统听写。
pub struct ConfiguredRecognizer {
    session: WeakEntity<Session>,
    system: SystemRecognizer,
    /// 语音请求专用运行时；界面线程不是 Tokio 线程。
    runtime: Option<Arc<tokio::runtime::Runtime>>,
}

impl ConfiguredRecognizer {
    /// 构造识别器。
    pub fn new(session: WeakEntity<Session>) -> Self {
        Self {
            session,
            system: SystemRecognizer::new(),
            runtime: tokio::runtime::Runtime::new().ok().map(Arc::new),
        }
    }

    /// 当前后端的可用性。
    fn backend(&self, cx: &App) -> (AudioBackend, Option<CloudConfig>) {
        let Some(session) = self.session.upgrade() else {
            return (AudioBackend::Off, None);
        };
        let config = session.read(cx).config().clone();
        let asr = config.audio.asr.clone();
        if asr.backend != AudioBackend::Cloud {
            return (asr.backend, None);
        }
        let Some(provider) = config.provider_named(asr.provider.trim()) else {
            return (AudioBackend::Off, None);
        };
        if !provider.is_configured() || asr.model.trim().is_empty() {
            return (AudioBackend::Off, None);
        }
        let dashscope = provider.endpoint().contains("dashscope.aliyuncs.com")
            || asr.endpoint.contains("dashscope.aliyuncs.com");
        let endpoint = if !asr.endpoint.trim().is_empty() {
            asr.endpoint.trim().to_owned()
        } else if dashscope {
            format!(
                "{}/api/v1/services/aigc/multimodal-generation/generation",
                service_root(&provider.endpoint())
            )
        } else {
            format!("{}/audio/transcriptions", provider.endpoint().trim_end_matches('/'))
        };
        let Some(runtime) = self.runtime.clone() else {
            return (AudioBackend::Off, None);
        };
        (
            AudioBackend::Cloud,
            Some(CloudConfig {
                endpoint,
                api_key: provider.api_key.clone(),
                model: asr.model.trim().to_owned(),
                language: asr.language.trim().to_owned(),
                dashscope,
                headers: provider.headers.clone(),
                runtime: Some(runtime),
            }),
        )
    }
}

impl SpeechRecognizer for ConfiguredRecognizer {
    fn audio_format(&self) -> AudioFormat {
        AudioFormat::default()
    }

    fn is_available(&self, cx: &App) -> bool {
        let (backend, cloud) = self.backend(cx);
        (backend == AudioBackend::Cloud && cloud.is_some())
            || (backend == AudioBackend::System && cfg!(any(windows, target_os = "macos")))
    }

    fn start(
        &self,
        sink: SpeechSink,
        cx: &mut App,
    ) -> Result<Box<dyn RecognitionSession>, SpeechError> {
        match self.backend(cx) {
            (AudioBackend::Cloud, Some(config)) => Ok(Box::new(CloudSession::new(sink, config))),
            (AudioBackend::System, _) => self.system.start(sink, cx),
            _ => Err(SpeechError::Unsupported),
        }
    }
}

/// 一次云端识别会话。
struct CloudSession {
    sink: SpeechSink,
    config: CloudConfig,
    samples: Vec<i16>,
}

impl CloudSession {
    fn new(sink: SpeechSink, config: CloudConfig) -> Self {
        Self {
            sink,
            config,
            samples: Vec::new(),
        }
    }
}

impl RecognitionSession for CloudSession {
    fn push_audio(&mut self, samples: &[i16], _cx: &mut App) {
        self.samples.extend_from_slice(samples);
    }

    fn finish(&mut self, cx: &mut App) {
        let wav = crate::audio::wav::encode_pcm16(&self.samples, 16_000, 1);
        let sink = self.sink.clone();
        let config = self.config.clone();
        if self.samples.is_empty() {
            sink.finish(cx);
            return;
        }

        let Some(runtime) = config.runtime.clone() else {
            sink.error(
                SpeechError::recognizer(std::io::Error::other("speech runtime is unavailable")),
                cx,
            );
            return;
        };
        cx.spawn(async move |cx| {
            let task = cx
                .background_executor()
                .spawn(async move { runtime.block_on(transcribe(&config, wav)) });
            let result = task.await;
            cx.update(|cx| match result {
                Ok(text) if !text.trim().is_empty() => {
                    sink.phrase(text, cx);
                    sink.finish(cx);
                }
                Ok(_) => sink.finish(cx),
                Err(error) => sink.error(SpeechError::recognizer(std::io::Error::other(error)), cx),
            });
        })
        .detach();
    }
}

/// 调用 `audio/transcriptions` 并返回文本。
async fn transcribe(config: &CloudConfig, wav: Vec<u8>) -> Result<String, String> {
    if config.dashscope {
        return dashscope_transcribe(config, wav).await;
    }
    let part = reqwest::multipart::Part::bytes(wav)
        .file_name("speech.wav")
        .mime_str("audio/wav")
        .map_err(|error| error.to_string())?;
    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", config.model.clone())
        .text("response_format", "json");
    if !config.language.is_empty() {
        form = form.text("language", config.language.clone());
    }

    let mut request = reqwest::Client::new()
        .post(&config.endpoint)
        .timeout(std::time::Duration::from_secs(120))
        .bearer_auth(&config.api_key);
    for (name, value) in &config.headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = request.multipart(form).send().await.map_err(|error| error.to_string())?;
    let status = response.status();
    let body = response.text().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!("transcription failed with status {status}: {body}"));
    }
    #[derive(serde::Deserialize)]
    struct Transcription {
        #[serde(default)]
        text: String,
    }
    let parsed: Transcription =
        serde_json::from_str(&body).map_err(|error| format!("invalid transcription: {error}"))?;
    Ok(parsed.text)
}

/// 百炼原生多模态接口的转写。
async fn dashscope_transcribe(config: &CloudConfig, wav: Vec<u8>) -> Result<String, String> {
    let audio = format!("data:audio/wav;base64,{}", agent::tools::media::encode_base64(&wav));
    let parameters = if config.language.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::json!({ "asr_options": { "language": config.language } })
    };
    let body = serde_json::json!({
        "model": config.model,
        "input": { "messages": [{ "role": "user", "content": [{ "audio": audio }] }] },
        "parameters": parameters,
    });
    let response = reqwest::Client::new()
        .post(&config.endpoint)
        .timeout(std::time::Duration::from_secs(120))
        .bearer_auth(&config.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let text = response.text().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!("transcription failed with status {status}: {text}"));
    }
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| format!("invalid transcription: {error}"))?;
    value
        .pointer("/output/choices/0/message/content")
        .and_then(|content| content.as_array())
        .and_then(|items| {
            items.iter().find_map(|item| item.get("text").and_then(|text| text.as_str()))
        })
        .map(str::to_owned)
        .ok_or_else(|| format!("transcription response has no text: {text}"))
}

/// 服务根地址（scheme + host）。
fn service_root(endpoint: &str) -> String {
    let trimmed = endpoint.trim_end_matches('/');
    let Some(scheme) = trimmed.find("://") else {
        return trimmed.to_owned();
    };
    let rest = &trimmed[scheme + 3..];
    match rest.find('/') {
        Some(path) => trimmed[..scheme + 3 + path].to_owned(),
        None => trimmed.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{CloudConfig, dashscope_transcribe};
    use crate::audio::tts::{TtsSettings, synthesize};

    fn bailian_key() -> Option<String> {
        let path = std::env::var_os("HALOETH_TEST_KEYS_FILE")?;
        let text = std::fs::read_to_string(path).ok()?;
        text.lines()
            .find_map(|line| line.strip_prefix("阿里云千问平台通用："))
            .map(|key| key.trim().to_owned())
    }

    /// 真实调用百炼 TTS 与 ASR，验证两端的请求与解析。
    #[tokio::test]
    #[ignore = "requires HALOETH_TEST_KEYS_FILE and network"]
    async fn live_dashscope_audio_roundtrip() {
        let Some(key) = bailian_key() else {
            return;
        };
        let settings = TtsSettings {
            backend: crate::config::AudioBackend::Cloud,
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".to_owned(),
            api_key: key.clone(),
            model: "qwen3-tts-flash".to_owned(),
            voice: "Cherry".to_owned(),
            rate: 0,
            volume: 100,
            endpoint: String::new(),
            headers: Default::default(),
        };
        let wav =
            tokio::task::spawn_blocking(move || synthesize(&settings, "你好，这是语音自检。"))
                .await
                .expect("tts task")
                .expect("tts");
        assert!(wav.len() > 1000);
        let config = CloudConfig {
            endpoint:
                "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation"
                    .to_owned(),
            api_key: key,
            model: "qwen3-asr-flash".to_owned(),
            language: "zh".to_owned(),
            dashscope: true,
            headers: Default::default(),
            runtime: None,
        };
        let text = dashscope_transcribe(&config, wav).await.expect("asr");
        println!("asr: {text}");
        assert!(text.contains("语音") || !text.trim().is_empty());
    }
}
