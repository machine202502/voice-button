//! Audio helpers: a streaming resampler, a crash-safe WAV writer and a tolerant
//! reader used by recording recovery.

use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Streaming linear resampler for one channel. The fractional read position and
/// the last sample are carried across calls, so chunk boundaries do not click.
pub struct Resampler {
    step: f64,
    pos: f64,
    prev: f32,
    primed: bool,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Self {
            step: from as f64 / to as f64,
            pos: 0.0,
            prev: 0.0,
            primed: false,
        }
    }

    /// Reads `input` and appends the resampled samples to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        if !self.primed {
            self.prev = input[0];
            self.primed = true;
        }
        let n = input.len();
        // Virtual sample k: k == 0 is the last sample of the previous chunk,
        // k in 1..=n is input[k - 1].
        while self.pos < n as f64 {
            let i = self.pos.floor();
            let frac = (self.pos - i) as f32;
            let a = if i < 1.0 {
                self.prev
            } else {
                input[i as usize - 1]
            };
            let b = if i as usize >= n {
                input[n - 1]
            } else {
                input[i as usize]
            };
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.pos -= n as f64;
        self.prev = input[n - 1];
    }
}

/// Minimal RIFF/WAVE writer for 16-bit mono PCM.
///
/// The header is written up front with zero sizes and patched on `finish`. A
/// process that dies mid-recording leaves a file whose data is intact but whose
/// sizes are zero; [`read_wav`] trusts the real file length, so the audio is
/// still usable and the header can be repaired afterwards.
pub struct WavWriter {
    inner: BufWriter<File>,
    rate: u32,
    frames: u64,
    path: std::path::PathBuf,
}

pub const WAV_HEADER_BYTES: usize = 44;

impl WavWriter {
    pub fn create(path: &Path, rate: u32) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
        let mut writer = Self {
            inner: BufWriter::new(file),
            rate,
            frames: 0,
            path: path.to_path_buf(),
        };
        writer.inner.write_all(&header(rate, 0))?;
        Ok(writer)
    }

    pub fn write(&mut self, samples: &[f32]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            let value = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        self.inner.write_all(&bytes)?;
        self.inner.flush()?;
        self.frames += samples.len() as u64;
        Ok(())
    }

    pub fn seconds(&self) -> f64 {
        self.frames as f64 / self.rate.max(1) as f64
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// Patch the sizes and close the file.
    pub fn finish(mut self) -> Result<()> {
        self.inner.flush()?;
        let mut file = self
            .inner
            .into_inner()
            .map_err(|e| anyhow::anyhow!("write {}: {e}", self.path.display()))?;
        patch_header(&mut file, self.rate, self.frames)?;
        file.flush()?;
        Ok(())
    }
}

fn header(rate: u32, frames: u64) -> Vec<u8> {
    let data = (frames * 2) as u32;
    let mut out = Vec::with_capacity(WAV_HEADER_BYTES);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    out
}

fn patch_header(file: &mut File, rate: u32, frames: u64) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header(rate, frames))?;
    Ok(())
}

/// Samples and rate read back from a WAV file.
pub struct WavData {
    pub samples: Vec<f32>,
    pub rate: u32,
}

/// Read a 16-bit PCM WAV, tolerating a zero or stale data size (the state a
/// crashed writer leaves behind).
pub fn read_wav(path: &Path) -> Result<WavData> {
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .read_to_end(&mut bytes)?;
    if bytes.len() < WAV_HEADER_BYTES || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        anyhow::bail!("{} is not a RIFF/WAVE file", path.display());
    }
    // Walk the chunks: some writers add a LIST chunk before `data`.
    let mut pos = 12usize;
    let mut rate = 16_000u32;
    let mut channels = 1u16;
    let mut data_start = None;
    let mut declared = 0usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]])
            as usize;
        if id == b"fmt " {
            if pos + 8 + 16 <= bytes.len() {
                channels = u16::from_le_bytes([bytes[pos + 10], bytes[pos + 11]]).max(1);
                rate = u32::from_le_bytes([
                    bytes[pos + 12],
                    bytes[pos + 13],
                    bytes[pos + 14],
                    bytes[pos + 15],
                ]);
            }
        } else if id == b"data" {
            data_start = Some(pos + 8);
            declared = size;
            break;
        }
        pos += 8 + size + (size & 1);
    }
    let start = data_start.context("no data chunk in the WAV file")?;
    let available = bytes.len() - start;
    let usable = if declared == 0 || declared > available {
        available
    } else {
        declared
    };
    let frame_bytes = 2 * channels as usize;
    let frames = usable / frame_bytes;
    let mut samples = Vec::with_capacity(frames);
    for frame in 0..frames {
        let at = start + frame * frame_bytes;
        let mut sum = 0.0f32;
        for channel in 0..channels as usize {
            let offset = at + channel * 2;
            sum += i16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as f32 / 32768.0;
        }
        samples.push(sum / channels as f32);
    }
    Ok(WavData { samples, rate })
}

#[cfg(test)]
mod tests {
    use super::{read_wav, Resampler, WavWriter};

    fn wave(count: usize) -> Vec<f32> {
        (0..count)
            .map(|i| (i as f32 * 0.05).sin())
            .collect()
    }

    #[test]
    fn resampling_halves_the_length() {
        let mut resampler = Resampler::new(32_000, 16_000);
        let mut out = Vec::new();
        for chunk in wave(32_000).chunks(1_000) {
            resampler.process(chunk, &mut out);
        }
        let expected = 16_000;
        assert!(
            (out.len() as i64 - expected as i64).abs() < 40,
            "got {}",
            out.len()
        );
    }

    #[test]
    fn chunked_resampling_matches_one_shot() {
        let input = wave(4_800);
        let mut one = Vec::new();
        Resampler::new(48_000, 16_000).process(&input, &mut one);

        let mut chunked = Vec::new();
        let mut resampler = Resampler::new(48_000, 16_000);
        for chunk in input.chunks(160) {
            resampler.process(chunk, &mut chunked);
        }
        assert_eq!(one.len(), chunked.len());
        for (a, b) in one.iter().zip(&chunked) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn a_wav_written_without_finish_is_still_readable() {
        let dir = std::env::temp_dir().join("vb-wav-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("cut.wav");
        {
            let mut writer = WavWriter::create(&path, 16_000).expect("create");
            writer.write(&wave(3_200)).expect("write");
            // Dropped without `finish`, as after a crash.
        }
        let data = read_wav(&path).expect("read");
        assert_eq!(data.rate, 16_000);
        assert_eq!(data.samples.len(), 3_200);
    }

    #[test]
    fn finished_wav_has_no_extra_samples() {
        let dir = std::env::temp_dir().join("vb-wav-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("done.wav");
        let mut writer = WavWriter::create(&path, 16_000).expect("create");
        writer.write(&wave(1_000)).expect("write");
        writer.finish().expect("finish");
        assert_eq!(read_wav(&path).expect("read").samples.len(), 1_000);
    }
}
