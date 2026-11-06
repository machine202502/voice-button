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
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            stream: None,
            buffer: Arc::new(Mutex::new(Vec::new())),
            sample_rate: 48000,
            channels: 1,
        }
    }

    pub fn is_active(&self) -> bool {
        self.stream.is_some()
    }

    pub fn start(&mut self) -> Result<()> {
        if self.stream.is_some() {
            return Ok(());
        }
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("no default microphone found")?;
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

#[cfg(test)]
mod tests {
    use super::mono_frames;

    #[test]
    fn stereo_frames_become_one_channel() {
        assert_eq!(mono_frames(&[0.5, -0.5, 0.25, 0.25], 2), vec![0.0, 0.25]);
    }

    #[test]
    fn mono_input_passes_through() {
        assert_eq!(mono_frames(&[0.1, 0.2], 1), vec![0.1, 0.2]);
    }
}
