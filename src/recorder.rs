use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use std::sync::{Arc, Mutex};

pub struct Recording {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Fold interleaved device frames into one mono channel.
///
/// The default input device often delivers stereo (or more), and appending
/// L,R,L,R as if it were mono halves the pitch and scrambles both channels:
/// no recognizer can work with such a stream.
fn mono_frames(samples: &[f32], channels: usize) -> Vec<f32> {
    let frames = channels.max(1);
    samples
        .chunks(frames)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect()
}

pub struct Recorder {
    stream: Option<Stream>,
    buffer: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
    channels: u16,
    device_name: String,
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            stream: None,
            buffer: Arc::new(Mutex::new(Vec::new())),
            sample_rate: 48000,
            channels: 1,
            device_name: String::new(),
        }
    }

    pub fn is_active(&self) -> bool {
        self.stream.is_some()
    }

    pub fn start(&mut self, preferred: Option<&str>) -> Result<()> {
        if self.stream.is_some() {
            return Ok(());
        }
        let host = cpal::default_host();
        let requested = preferred.map(str::trim).filter(|name| !name.is_empty());
        let mut device = requested.and_then(|name| {
            host.input_devices().ok().and_then(|devices| {
                devices
                    .into_iter()
                    .find(|device| device.name().ok().as_deref() == Some(name))
            })
        });
        if device.is_none() {
            if let Some(name) = requested {
                crate::log::log(&format!(
                    "microphone {name:?} not found; using the system default device"
                ));
            }
            device = host.default_input_device();
        }
        let device = device.context("no default microphone found")?;
        self.device_name = device
            .name()
            .unwrap_or_else(|_| "unknown microphone".to_string());
        let supported = device
            .default_input_config()
            .context("failed to get microphone config")?;
        self.sample_rate = supported.sample_rate().0;
        self.channels = supported.channels().max(1);
        let config: StreamConfig = supported.config();
        self.buffer.lock().unwrap().clear();
        let buffer = self.buffer.clone();
        let format = supported.sample_format();
        let frames = self.channels as usize;
        let stream = match format {
            SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    let mono = mono_frames(data, frames);
                    buffer.lock().unwrap().extend(mono);
                },
                move |err| eprintln!("microphone error: {err}"),
                None,
            )?,
            SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    let converted: Vec<f32> =
                        data.iter().map(|&sample| sample as f32 / 32768.0).collect();
                    let mono = mono_frames(&converted, frames);
                    buffer.lock().unwrap().extend(mono);
                },
                move |err| eprintln!("microphone error: {err}"),
                None,
            )?,
            SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    let converted: Vec<f32> = data
                        .iter()
                        .map(|&sample| (sample as f32 - 32768.0) / 32768.0)
                        .collect();
                    let mono = mono_frames(&converted, frames);
                    buffer.lock().unwrap().extend(mono);
                },
                move |err| eprintln!("microphone error: {err}"),
                None,
            )?,
            other => bail!("unsupported microphone sample format {other:?}"),
        };
        stream.play()?;
        self.stream = Some(stream);
        Ok(())
    }

    /// Handle to the live sample buffer, for streaming transcription while
    /// recording is in progress.
    pub fn buffer_handle(&self) -> Arc<Mutex<Vec<f32>>> {
        self.buffer.clone()
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Name of the device used by the current (or last) recording.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn stop(&mut self) -> Result<Recording> {
        if self.stream.is_none() {
            bail!("not recording");
        }
        self.stream = None;
        let samples = std::mem::take(&mut *self.buffer.lock().unwrap());
        Ok(Recording {
            samples,
            sample_rate: self.sample_rate,
        })
    }
}

/// Names of devices that capture silence unless something is routed into them.
pub fn is_virtual_device_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        "cable",
        "vb-audio",
        "virtual",
        "streaming",
        "loopback",
        "stereo mix",
        "what u hear",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Name of the Windows default input device.
pub fn default_input_device_name() -> Option<String> {
    cpal::default_host()
        .default_input_device()
        .and_then(|device| device.name().ok())
}

/// First input device that is not a virtual cable/loopback.
pub fn first_real_input_device() -> Option<String> {
    input_device_names()
        .into_iter()
        .find(|name| !is_virtual_device_name(name))
}

/// Human-readable names of the available input devices (duplicates removed).
/// Used by Settings so the user can pick a real microphone instead of a
/// virtual cable, which records silence.
pub fn input_device_names() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for device in devices {
            if let Ok(name) = device.name() {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::{is_virtual_device_name, mono_frames};

    #[test]
    fn stereo_frames_become_one_channel() {
        assert_eq!(mono_frames(&[0.5, -0.5, 0.25, 0.25], 2), vec![0.0, 0.25]);
    }

    #[test]
    fn mono_input_passes_through() {
        assert_eq!(mono_frames(&[0.1, 0.2], 1), vec![0.1, 0.2]);
    }

    #[test]
    fn virtual_input_devices_are_recognized() {
        assert!(is_virtual_device_name(
            "CABLE Output (VB-Audio Virtual Cable)"
        ));
        assert!(is_virtual_device_name("AMD Streaming Audio Device"));
        assert!(is_virtual_device_name("Stereo Mix (Realtek(R) Audio)"));
        assert!(!is_virtual_device_name(
            "Набор микрофонов (Realtek(R) Audio)"
        ));
    }
}
