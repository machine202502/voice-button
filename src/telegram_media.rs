//! Decoding of Telegram attachments.
//!
//! Telegram voice notes are Ogg/Opus, which the app's main decoder
//! (Symphonia 0.5) cannot read. This module uses the 0.6 line with the
//! pure-Rust Opus decoder registered as a fallback, then resamples everything
//! to the rate the models expect. It streams packets, so a long file never has
//! to be decoded all at once in the sense of holding compressed data.

use crate::audio::Resampler;
use anyhow::{Context, Result};
use std::path::Path;
use symphonia06::core::codecs::audio::AudioDecoderOptions;
use symphonia06::core::codecs::registry::CodecRegistry;
use symphonia06::core::common::Tier;
use symphonia06::core::formats::probe::Hint;
use symphonia06::core::formats::{FormatOptions, TrackType};
use symphonia06::core::io::MediaSourceStream;
use symphonia06::core::meta::MetadataOptions;
use symphonia_opus::OpusDecoder;

pub const TARGET_RATE: u32 = 16_000;

/// Decode any supported container to mono f32 at 16 kHz.
pub fn decode_to_16k(path: &Path) -> Result<Vec<f32>> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
        hint.with_extension(extension);
    }

    let mut codecs = CodecRegistry::new();
    symphonia06::default::register_enabled_codecs(&mut codecs);
    codecs.register_audio_decoder_at_tier::<OpusDecoder>(Tier::Fallback);

    let mut format = symphonia06::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .with_context(|| format!("{} is not a supported media file", path.display()))?;

    let track = format
        .default_track(TrackType::Audio)
        .context("the file has no audio track")?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .context("the audio track has no codec parameters")?
        .clone();
    let mut decoder = codecs
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .context("no decoder for this audio codec")?;

    let mut samples = Vec::new();
    let mut rate = None;
    let mut resampler = None;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(error) => {
                // A truncated file still yields the packets before the break.
                if samples.is_empty() {
                    return Err(error).context("reading the audio stream");
                }
                break;
            }
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(_) => continue,
        };
        let spec = decoded.spec();
        let source_rate = spec.rate();
        let channels = spec.channels().count().max(1);
        let mut frames = Vec::new();
        decoded.copy_to_vec_interleaved::<f32>(&mut frames);
        if channels > 1 {
            let mono: Vec<f32> = frames
                .chunks(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                .collect();
            frames = mono;
        }
        if rate != Some(source_rate) {
            rate = Some(source_rate);
            resampler = (source_rate != TARGET_RATE).then(|| Resampler::new(source_rate, TARGET_RATE));
        }
        match resampler.as_mut() {
            Some(resampler) => resampler.process(&frames, &mut samples),
            None => samples.extend_from_slice(&frames),
        }
    }
    if samples.is_empty() {
        anyhow::bail!("the file contains no decodable audio");
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ogg_opus_voice_note_is_decoded() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("assets")
            .join("voice.opus");
        let samples = decode_to_16k(&path).expect("decode the opus fixture");
        let seconds = samples.len() as f32 / TARGET_RATE as f32;
        assert!(
            (seconds - 8.0).abs() < 0.5,
            "the voice note is about eight seconds, got {seconds}"
        );
    }

    /// A tiny 16 kHz mono WAV, so the container path is tested without a model.
    #[test]
    fn a_wav_file_is_decoded_to_samples() {
        let dir = std::env::temp_dir().join(format!(
            "vb-telegram-media-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.wav");
        let mut bytes = Vec::new();
        let seconds = 0.2f32;
        let frames = (16_000.0 * seconds) as u32;
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + frames * 2).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&16_000u32.to_le_bytes());
        bytes.extend_from_slice(&32_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(frames * 2).to_le_bytes());
        for _ in 0..frames {
            bytes.extend_from_slice(&1000i16.to_le_bytes());
        }
        std::fs::write(&path, &bytes).unwrap();
        let samples = decode_to_16k(&path).unwrap();
        assert!((samples.len() as f32 - 16_000.0 * seconds).abs() < 64.0);
        assert!(samples.iter().all(|sample| sample.abs() < 0.1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
