//! Decoding of user-provided audio files (WAV, MP3, FLAC, OGG, M4A/AAC …) and
//! of the audio track of a video file (MP4, MOV, MKV): a container may hold
//! several tracks, and only one of them has a decoder in this build.
//!
//! Everything symphonia supports is accepted; the app does not keep its own
//! format list, so a new codec in the library is available immediately. The
//! decoder streams: packets are decoded, downmixed and resampled to 16 kHz mono
//! one at a time, so a two-hour file never has to fit in memory.

use crate::audio::Resampler;
use anyhow::{Context, Result};
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Sample rate every model expects.
pub const TARGET_RATE: u32 = 16_000;

#[derive(Debug, Clone, Default)]
pub struct AudioInfo {
    pub rate: u32,
    pub channels: usize,
    /// Total length in seconds, when the container reports it.
    pub seconds: Option<f64>,
}

fn open(path: &Path) -> Result<Box<dyn symphonia::core::formats::FormatReader>> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .with_context(|| format!("{} is not a supported audio or video file", path.display()))?;
    Ok(probed.format)
}

/// The first track this build can actually decode.
///
/// A video file lists its picture track first; asking the codec registry is the
/// reliable way to skip it without hard-coding container-specific track ids.
fn audio_track(
    format: &dyn symphonia::core::formats::FormatReader,
) -> Option<symphonia::core::formats::Track> {
    format
        .tracks()
        .iter()
        .filter(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .find(|track| {
            symphonia::default::get_codecs()
                .make(&track.codec_params, &DecoderOptions::default())
                .is_ok()
        })
        .cloned()
}

/// Read the track parameters without decoding the audio.
pub fn probe(path: &Path) -> Result<AudioInfo> {
    let format = open(path)?;
    let track = audio_track(&*format).context("the file has no audio track")?;
    let rate = track
        .codec_params
        .sample_rate
        .context("the file does not report a sample rate")?;
    let channels = track
        .codec_params
        .channels
        .map(|channels| channels.count())
        .unwrap_or(1);
    let seconds = track
        .codec_params
        .n_frames
        .map(|frames| frames as f64 / rate as f64);
    Ok(AudioInfo {
        rate,
        channels,
        seconds,
    })
}

/// Decode the whole file, handing 16 kHz mono chunks to `sink`.
pub fn decode_file(path: &Path, mut sink: impl FnMut(&[f32])) -> Result<AudioInfo> {
    let mut format = open(path)?;
    let track = audio_track(&*format).context("the file has no audio track")?;
    let rate = track
        .codec_params
        .sample_rate
        .context("the file does not report a sample rate")?;
    let channels = track
        .codec_params
        .channels
        .map(|channels| channels.count())
        .unwrap_or(1)
        .max(1);
    let seconds = track
        .codec_params
        .n_frames
        .map(|frames| frames as f64 / rate as f64);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("no decoder for this audio format")?;

    let mut resampler = (rate != TARGET_RATE).then(|| Resampler::new(rate, TARGET_RATE));
    let mut mono = Vec::new();
    let mut out = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(error) => return Err(anyhow::anyhow!("read the audio stream: {error}")),
        };
        if packet.track_id() != track.id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // A damaged frame in the middle is skipped, not fatal.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(error) => return Err(anyhow::anyhow!("decode the audio: {error}")),
        };
        let spec = *decoded.spec();
        let source_channels = spec.channels.count().max(1);
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        mono.clear();
        mono.reserve(buffer.samples().len() / source_channels);
        for frame in buffer.samples().chunks(source_channels) {
            mono.push(frame.iter().sum::<f32>() / source_channels as f32);
        }
        out.clear();
        match resampler.as_mut() {
            Some(resampler) => resampler.process(&mono, &mut out),
            None => out.extend_from_slice(&mono),
        }
        if !out.is_empty() {
            sink(&out);
        }
    }
    Ok(AudioInfo {
        rate,
        channels,
        seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::probe;
    use crate::audio::{read_wav, WavWriter};
    use crate::decode;

    fn tone(rate: u32, seconds: f32) -> Vec<f32> {
        (0..(rate as f32 * seconds) as usize)
            .map(|i| (i as f32 * 0.05).sin() * 0.3)
            .collect()
    }

    #[test]
    fn a_wav_file_decodes_to_16k_mono() {
        let dir = std::env::temp_dir().join("vb-decode-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("tone.wav");
        let mut writer = WavWriter::create(&path, 48_000).expect("wav");
        writer.write(&tone(48_000, 2.0)).expect("write");
        writer.finish().expect("finish");

        let info = probe(&path).expect("probe");
        assert_eq!(info.rate, 48_000);

        let mut total = 0usize;
        let mut peak = 0.0f32;
        decode::decode_file(&path, |samples| {
            total += samples.len();
            for sample in samples {
                peak = peak.max(sample.abs());
            }
        })
        .expect("decode");
        // Two seconds at 16 kHz, within the resampler's rounding.
        assert!(
            (total as i64 - 32_000).abs() < 200,
            "decoded {total} samples"
        );
        assert!(peak > 0.2, "silence instead of the tone: {peak}");
        let original = read_wav(&path).expect("read back");
        assert_eq!(original.samples.len(), 96_000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
