//! 可选设备的麦克风输入：按设置里的设备名采集，默认跟随系统。
//!
//! 设备格式会混为单声道并重采样到识别器要求的格式。实现参考 gpui-component
//! 的默认 `Microphone`，额外支持按名称选择设备。

use std::f32::consts::TAU;

use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, StreamConfig,
    traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _},
};
use gpui_kit::component::speech::{AudioFormat, AudioInput, AudioSink, SpeechError};
use gpui_kit::*;
use smol::channel::{Sender, unbounded};

use crate::session::Session;

/// 按配置选择设备的麦克风。
pub struct SelectableMicrophone {
    session: WeakEntity<Session>,
}

impl SelectableMicrophone {
    /// 构造输入。
    pub fn new(session: WeakEntity<Session>) -> Self {
        Self {
            session,
        }
    }
}

/// 列出系统输入设备名称。
pub fn input_devices() -> Vec<String> {
    let host = cpal::default_host();
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    let mut names: Vec<String> =
        devices.filter_map(|device| device.name().ok()).filter(|name| !name.is_empty()).collect();
    names.sort();
    names.dedup();
    names
}

impl AudioInput for SelectableMicrophone {
    fn start(
        &self,
        format: AudioFormat,
        sink: AudioSink,
        cx: &mut App,
    ) -> Result<Subscription, SpeechError> {
        let requested = self
            .session
            .upgrade()
            .map(|session| session.read(cx).config().audio.asr.input_device.clone())
            .unwrap_or_default();
        let host = cpal::default_host();
        let device = if requested.trim().is_empty() {
            None
        } else {
            host.input_devices().ok().and_then(|mut devices| {
                devices.find(|device| device.name().map(|name| name == requested).unwrap_or(false))
            })
        }
        .or_else(|| host.default_input_device())
        .ok_or(SpeechError::NoInputDevice)?;
        let supported = device.default_input_config().map_err(SpeechError::input)?;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let source_rate = config.sample_rate.0;

        let (tx, rx) = unbounded();
        let stream = match sample_format {
            SampleFormat::F32 => build_stream::<f32>(&device, &config, tx),
            SampleFormat::I16 => build_stream::<i16>(&device, &config, tx),
            SampleFormat::U16 => build_stream::<u16>(&device, &config, tx),
            SampleFormat::I32 => build_stream::<i32>(&device, &config, tx),
            other => {
                return Err(SpeechError::input(std::io::Error::other(format!(
                    "unsupported sample format {other}"
                ))));
            }
        }?;
        stream.play().map_err(SpeechError::input)?;

        let task = cx.spawn(async move |cx| {
            let mut converter = Converter::new(source_rate, format);
            let mut mono = Vec::new();
            while let Ok(first) = rx.recv().await {
                let mut error = None;
                for capture in
                    std::iter::once(first).chain(std::iter::from_fn(|| rx.try_recv().ok()))
                {
                    match capture {
                        Capture::Samples(samples) => mono.extend_from_slice(&samples),
                        Capture::Error(message) => error = Some(message),
                    }
                }

                let samples = converter.convert(&mono);
                mono.clear();
                cx.update(|cx| {
                    if !samples.is_empty() {
                        sink.push(samples, cx);
                    }
                    if let Some(message) = error.take() {
                        sink.error(SpeechError::input(std::io::Error::other(message)), cx);
                    }
                });
            }
        });

        Ok(Subscription::new(move || {
            drop(stream);
            drop(task);
        }))
    }
}

enum Capture {
    Samples(Vec<f32>),
    Error(String),
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    tx: Sender<Capture>,
) -> Result<cpal::Stream, SpeechError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let error_tx = tx.clone();
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let mono = data
                    .chunks(channels)
                    .map(|frame| {
                        frame.iter().map(|&sample| f32::from_sample(sample)).sum::<f32>()
                            / frame.len() as f32
                    })
                    .collect();
                let _ = tx.try_send(Capture::Samples(mono));
            },
            move |error| {
                let _ = error_tx.try_send(Capture::Error(error.to_string()));
            },
            None,
        )
        .map_err(|error| match error {
            cpal::BuildStreamError::DeviceNotAvailable => SpeechError::NoInputDevice,
            error => SpeechError::input(error),
        })
}

/// 单声道 f32 → 目标采样率的 i16 转换器。
struct Converter {
    step: f64,
    position: f64,
    previous: f32,
    low_pass: Option<LowPass>,
    channels: usize,
}

impl Converter {
    fn new(source_rate: u32, format: AudioFormat) -> Self {
        let target_rate = format.sample_rate().max(1);
        let source_rate = source_rate.max(1);
        Self {
            step: source_rate as f64 / target_rate as f64,
            position: 1.,
            previous: 0.,
            low_pass: (source_rate > target_rate)
                .then(|| LowPass::new(target_rate as f32 * 0.45, source_rate as f32)),
            channels: format.channels().max(1) as usize,
        }
    }

    fn convert(&mut self, input: &[f32]) -> Vec<i16> {
        if input.is_empty() {
            return Vec::new();
        }
        let filtered;
        let input = match self.low_pass.as_mut() {
            Some(low_pass) => {
                filtered = input.iter().map(|&x| low_pass.process(x)).collect::<Vec<_>>();
                &filtered[..]
            }
            None => input,
        };

        let sample = |ix: usize| {
            if ix == 0 {
                self.previous
            } else {
                input[ix - 1]
            }
        };
        let last = input.len();
        let len = last as f64;
        let mut output = Vec::with_capacity(((len / self.step) as usize + 1) * self.channels);
        while self.position <= len {
            let ix = self.position.floor() as usize;
            let frac = (self.position - ix as f64) as f32;
            let next = sample((ix + 1).min(last));
            let value = sample(ix) * (1. - frac) + next * frac;
            let value = (value.clamp(-1., 1.) * i16::MAX as f32) as i16;
            output.extend(std::iter::repeat_n(value, self.channels));
            self.position += self.step;
        }
        self.position -= len;
        self.previous = input[input.len() - 1];
        output
    }
}

struct LowPass {
    alpha: f32,
    stages: [f32; 2],
}

impl LowPass {
    fn new(cutoff: f32, sample_rate: f32) -> Self {
        Self {
            alpha: 1. - (-TAU * cutoff / sample_rate).exp(),
            stages: [0.; 2],
        }
    }

    fn process(&mut self, mut x: f32) -> f32 {
        for stage in &mut self.stages {
            *stage += self.alpha * (x - *stage);
            x = *stage;
        }
        x
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioFormat, Converter};

    #[test]
    fn converter_downsamples_across_chunks() {
        let mut converter = Converter::new(48_000, AudioFormat::default());
        let total: usize = (0..10).map(|_| converter.convert(&[0.; 480]).len()).sum();
        assert_eq!(total, 1_600);
    }
}
