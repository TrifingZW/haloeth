//! 语音播报：Windows SAPI 离线合成，或调用提供商的 `audio/speech` 接口。
//!
//! 合成与播放都在专用工作线程上执行：SAPI 走 COM，云端的 WAV 用系统
//! `PlaySound` 播放。界面线程只发送「朗读 / 停止」命令，不会阻塞。

use std::sync::mpsc::{Sender, channel};

use crate::config::AudioBackend;

/// 一次播报使用的配置快照。
#[derive(Debug, Clone)]
pub struct TtsSettings {
    /// 后端。
    pub backend: AudioBackend,
    /// 云端接口基地址。
    pub base_url: String,
    /// **API** 密钥。
    pub api_key: String,
    /// 模型标识。
    pub model: String,
    /// 系统语音名；空表示默认。
    pub voice: String,
    /// 语速 -10..10。
    pub rate: i32,
    /// 音量 0..100。
    pub volume: u8,
    /// 接口地址覆盖。
    pub endpoint: String,
    /// 附加请求头。
    pub headers: std::collections::BTreeMap<String, String>,
}

/// 工作线程命令。
enum Command {
    /// 朗读文本。
    Speak(String, TtsSettings),
    /// 停止当前朗读。
    Stop,
}

/// 语音播报服务；首次使用时创建工作线程。
pub struct TtsService {
    sender: Option<Sender<Command>>,
}

impl Default for TtsService {
    fn default() -> Self {
        Self::new()
    }
}

impl TtsService {
    /// 构造服务。
    pub fn new() -> Self {
        Self {
            sender: None,
        }
    }

    /// 工作线程是否已启动。
    #[allow(dead_code)]
    pub fn is_running(&self) -> bool {
        self.sender.is_some()
    }

    /// 是否配置了可用的播报后端。
    pub fn is_configured(settings: &TtsSettings) -> bool {
        match settings.backend {
            AudioBackend::Off => false,
            AudioBackend::System => cfg!(windows),
            AudioBackend::Cloud => {
                !settings.base_url.trim().is_empty() && !settings.model.trim().is_empty()
            }
        }
    }

    /// 朗读文本；未配置时什么也不做。
    pub fn speak(&mut self, text: impl Into<String>, settings: TtsSettings) {
        if !Self::is_configured(&settings) {
            return;
        }
        if self.sender.is_none() {
            let (sender, receiver) = channel();
            match std::thread::Builder::new()
                .name("haloeth-tts".to_owned())
                .spawn(move || worker(receiver))
            {
                Ok(_) => self.sender = Some(sender),
                Err(error) => {
                    tracing::warn!("无法启动语音播报线程：{error}");
                    return;
                }
            }
        }
        if let Some(sender) = &self.sender {
            let _ = sender.send(Command::Speak(text.into(), settings));
        }
    }

    /// 停止当前朗读。
    pub fn stop(&mut self) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(Command::Stop);
        }
    }
}

/// 工作线程主循环。
#[cfg(windows)]
fn worker(receiver: std::sync::mpsc::Receiver<Command>) {
    use windows::Win32::Media::Audio::{
        PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT, SND_PURGE,
    };
    use windows::Win32::Media::Speech::{ISpVoice, SPF_ASYNC, SPF_PURGEBEFORESPEAK, SpVoice};
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::core::{HSTRING, PCWSTR};

    // COM 只在这个线程上初始化；SAPI 对象也只在本地持有。
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let voice: Option<ISpVoice> = unsafe { CoCreateInstance(&SpVoice, None, CLSCTX_ALL).ok() };
    // `PlaySound` 的内存模式要求缓冲区在播放期间保持有效。
    // SAPI 的异步播报可能在调用返回后继续读取文本指针，必须保留字符串。
    #[allow(unused_assignments)]
    let mut spoken: Option<HSTRING> = None;
    // 云端 WAV 用临时文件播放：winmm 的异步内存模式在部分机器上会崩溃。
    let mut audio_path: Option<std::path::PathBuf> = None;
    let mut audio_name: Option<HSTRING> = None;

    while let Ok(command) = receiver.recv() {
        match command {
            Command::Stop => unsafe {
                if let Some(voice) = &voice {
                    let _ = voice.Speak(PCWSTR::null(), SPF_PURGEBEFORESPEAK.0 as u32, None);
                }
                let _ = PlaySoundW(PCWSTR::null(), None, SND_PURGE);
                let _ = spoken.take();
                let _ = audio_name.take();
                if let Some(path) = audio_path.take() {
                    let _ = std::fs::remove_file(path);
                }
            },
            Command::Speak(text, settings) => match settings.backend {
                AudioBackend::Off => {}
                AudioBackend::System => {
                    let Some(voice) = &voice else {
                        continue;
                    };
                    unsafe {
                        let _ = voice.SetRate(settings.rate.clamp(-10, 10));
                        let _ = voice.SetVolume(settings.volume.min(100) as u16);
                        let _ = voice.Speak(PCWSTR::null(), SPF_PURGEBEFORESPEAK.0 as u32, None);
                        spoken = Some(HSTRING::from(text));
                        if let Some(text) = &spoken
                            && let Err(error) =
                                voice.Speak(PCWSTR(text.as_ptr()), SPF_ASYNC.0 as u32, None)
                        {
                            tracing::warn!("SAPI 播报失败：{error}");
                        }
                    }
                }
                AudioBackend::Cloud => match synthesize(&settings, &text) {
                    Ok(bytes) => {
                        unsafe {
                            // 先停止上一段，再替换临时文件。
                            let _ = PlaySoundW(PCWSTR::null(), None, SND_PURGE);
                        }
                        let _ = audio_name.take();
                        if let Some(path) = audio_path.take() {
                            let _ = std::fs::remove_file(path);
                        }
                        let path = std::env::temp_dir()
                            .join(format!("haloeth-tts-{}.wav", std::process::id()));
                        let fixed = normalize_wav(&bytes).unwrap_or_else(|| bytes.clone());
                        if let Err(error) = std::fs::write(&path, &fixed) {
                            tracing::warn!("写入临时音频失败：{error}");
                            continue;
                        }
                        let name = HSTRING::from(path.to_string_lossy().as_ref());
                        unsafe {
                            let _ = PlaySoundW(
                                PCWSTR(name.as_ptr()),
                                None,
                                SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
                            );
                        }
                        audio_path = Some(path);
                        audio_name = Some(name);
                    }
                    Err(error) => tracing::warn!("云端语音合成失败：{error}"),
                },
            },
        }
    }
}

/// 非 Windows 平台暂不提供播报。
#[cfg(not(windows))]
fn worker(receiver: std::sync::mpsc::Receiver<Command>) {
    while let Ok(command) = receiver.recv() {
        if let Command::Speak(_, _) = command {
            tracing::warn!("当前平台暂不支持语音播报");
        }
    }
}

/// 调用 OpenAI 兼容的 `audio/speech`，返回 WAV 数据。
#[cfg(windows)]
pub(crate) fn synthesize(settings: &TtsSettings, text: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::new();
    let with_headers = |request: reqwest::blocking::RequestBuilder| {
        let mut request = request;
        for (name, value) in &settings.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        request
    };
    let dashscope = settings.base_url.contains("dashscope.aliyuncs.com")
        || settings.endpoint.contains("dashscope.aliyuncs.com");
    let endpoint = if !settings.endpoint.trim().is_empty() {
        settings.endpoint.trim().to_owned()
    } else if dashscope {
        format!(
            "{}/api/v1/services/aigc/multimodal-generation/generation",
            service_root(&settings.base_url)
        )
    } else {
        format!("{}/audio/speech", settings.base_url.trim_end_matches('/'))
    };

    if dashscope {
        let body = serde_json::json!({
            "model": settings.model,
            "input": {
                "text": text.chars().take(600).collect::<String>(),
                "voice": if settings.voice.is_empty() { "Cherry" } else { &settings.voice },
                "language_type": "Auto",
            }
        });
        let response = with_headers(client.post(endpoint))
            .timeout(std::time::Duration::from_secs(120))
            .bearer_auth(&settings.api_key)
            .json(&body)
            .send()
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let body = response.text().map_err(|error| error.to_string())?;
        if !status.is_success() {
            return Err(format!("speech request failed with status {status}: {body}"));
        }
        let value: serde_json::Value =
            serde_json::from_str(&body).map_err(|error| error.to_string())?;
        if let Some(data) = value.pointer("/output/audio/data").and_then(|data| data.as_str())
            && !data.is_empty()
        {
            return agent::tools::media::decode_base64(data).map_err(|error| error.to_string());
        }
        let url = value
            .pointer("/output/audio/url")
            .and_then(|url| url.as_str())
            .filter(|url| !url.is_empty())
            .ok_or_else(|| format!("speech response has no audio url: {body}"))?;
        let response = client
            .get(url)
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(format!("downloading audio failed: {}", response.status()));
        }
        return response.bytes().map(|bytes| bytes.to_vec()).map_err(|error| error.to_string());
    }

    let body = serde_json::json!({
        "model": settings.model,
        "input": text,
        "voice": if settings.voice.is_empty() { "alloy" } else { &settings.voice },
        "response_format": "wav",
    });
    let response = with_headers(client.post(endpoint))
        .timeout(std::time::Duration::from_secs(120))
        .bearer_auth(&settings.api_key)
        .json(&body)
        .send()
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("speech request failed: {}", response.status()));
    }
    response.bytes().map(|bytes| bytes.to_vec()).map_err(|error| error.to_string())
}

/// 修正 WAV 头里不可信的长度字段。
///
/// 百炼返回的 WAV 把 RIFF 与 data 长度写成 `0x7FFFFFFF`，Windows 的
/// `PlaySound` 会因此拒绝播放；这里按实际字节数重写这两个字段。
pub(crate) fn normalize_wav(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut output = bytes.to_vec();
    let riff_len = (bytes.len() - 8) as u32;
    output[4..8].copy_from_slice(&riff_len.to_le_bytes());
    // 找到 data 块并修正其长度。
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        if id == b"data" {
            let available = bytes.len().saturating_sub(offset + 8) as u32;
            output[offset + 4..offset + 8].copy_from_slice(&available.to_le_bytes());
            return Some(output);
        }
        if size == 0 || offset + 8 + size > bytes.len() {
            break;
        }
        offset += 8 + size + (size & 1);
    }
    Some(output)
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
    use super::*;

    #[test]
    fn wav_lengths_are_rewritten() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&0x7fff_ffffu32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&24_000u32.to_le_bytes());
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&0x7fff_ffffu32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 8]);

        let fixed = normalize_wav(&bytes).expect("wav");
        assert_eq!(u32::from_le_bytes(fixed[4..8].try_into().unwrap()), bytes.len() as u32 - 8);
        assert_eq!(u32::from_le_bytes(fixed[40..44].try_into().unwrap()), 8);
        assert!(normalize_wav(b"not a wav").is_none());
    }

    #[test]
    fn configuration_requires_a_backend() {
        let mut settings = TtsSettings {
            backend: AudioBackend::Off,
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            voice: String::new(),
            rate: 0,
            volume: 100,
            endpoint: String::new(),
            headers: Default::default(),
        };
        assert!(!TtsService::is_configured(&settings));

        settings.backend = AudioBackend::Cloud;
        assert!(!TtsService::is_configured(&settings));
        settings.base_url = "https://example.com/v1".into();
        settings.model = "tts-1".into();
        assert!(TtsService::is_configured(&settings));
    }

    #[test]
    fn speak_is_a_noop_when_unconfigured() {
        let mut service = TtsService::new();
        service.speak(
            "hello",
            TtsSettings {
                backend: AudioBackend::Off,
                base_url: String::new(),
                api_key: String::new(),
                model: String::new(),
                voice: String::new(),
                rate: 0,
                volume: 100,
                endpoint: String::new(),
                headers: Default::default(),
            },
        );
        assert!(!service.is_running());
    }
}
