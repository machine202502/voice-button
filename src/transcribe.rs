use crate::config::{AppDirs, Backend, Config};
use crate::model_manager;
use crate::recorder::Recording;
use anyhow::{anyhow, bail, Context, Result};
use sherpa_onnx::{
    OfflineModelConfig, OfflineNemoEncDecCtcModelConfig, OfflineQwen3ASRModelConfig,
    OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig,
    OfflineTransducerModelConfig, OfflineWhisperModelConfig,
};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::model_manager::OnnxFamily;

pub trait Transcriber: Send {
    fn transcribe(&self, recording: Recording) -> Result<String>;
}

/// CPAL records at the device rate (usually 48 kHz); every recognizer here wants
/// 16 kHz mono float samples.
pub fn resample_to_16k(samples: Vec<f32>, from: u32) -> Vec<f32> {
    if samples.is_empty() || from == 16000 {
        return samples;
    }
    let src = from as f64;
    let to = 16000.0f64;
    let out_len = ((samples.len() as f64) * to / src).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let t = i as f64 * src / to;
        let i0 = (t.floor() as usize).min(samples.len() - 1);
        let i1 = (i0 + 1).min(samples.len() - 1);
        let w = (t - i0 as f64) as f32;
        out.push(samples[i0] * (1.0 - w) + samples[i1] * w);
    }
    out
}

/// GigaAM (NeMo CTC) and the RNN-T variants, driven by the sherpa-onnx engine.
///
/// The engine owns the whole frontend — pre-emphasis, windowing, log-mel and
/// the per-feature normalization the checkpoint was trained with — and both
/// decoders. Hand-rolling any part of that (as this app used to) offsets the
/// features the model expects and costs accuracy, especially on quiet speech
/// and across segment boundaries.
pub struct SherpaTranscriber {
    recognizer: OfflineRecognizer,
}

/// sherpa-onnx reads `vocab_size` and `subsampling_factor` from the ONNX model
/// metadata and terminates the whole process when they are missing, which takes
/// the app down with no message at all. Exports made for other runtimes (the
/// istupakov GigaAM files, for one) carry no such metadata, so look at the file
/// before handing it to the engine and report a normal error instead.
fn ensure_sherpa_metadata(path: &Path) -> Result<()> {
    const NEEDLES: [&[u8]; 2] = [b"vocab_size", b"subsampling_factor"];
    // ModelProto serializes the graph before its metadata, so the tail of the
    // file almost always answers the question; a full scan is the fallback for
    // exporters that write the metadata first.
    if scan_for_metadata(path, &NEEDLES, 16 << 20)? || scan_for_metadata(path, &NEEDLES, 0)? {
        return Ok(());
    }
    bail!(
        "{} carries no sherpa-onnx metadata (vocab_size, subsampling_factor): it was \
         exported for another runtime and this engine cannot load it. Pick a model from \
         the sherpa-onnx conversions, for example GigaAM v3 (sherpa-onnx).",
        path.display()
    )
}

/// Whether every needle occurs in the file, scanning the whole file when
/// `tail_bytes` is zero and only its last `tail_bytes` otherwise.
fn scan_for_metadata(path: &Path, needles: &[&[u8]], tail_bytes: u64) -> Result<bool> {
    use std::io::{Seek, SeekFrom};
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let length = file.metadata()?.len();
    if tail_bytes > 0 && length > tail_bytes {
        file.seek(SeekFrom::Start(length - tail_bytes))?;
    }
    let mut found = vec![false; needles.len()];
    let mut buffer = vec![0u8; 1 << 20];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let read = file.read(&mut buffer).context("cannot read the model file")?;
        if read == 0 {
            break;
        }
        let mut window = std::mem::take(&mut carry);
        window.extend_from_slice(&buffer[..read]);
        for (index, needle) in needles.iter().enumerate() {
            if !found[index] && window.windows(needle.len()).any(|chunk| chunk == *needle) {
                found[index] = true;
            }
        }
        if found.iter().all(|value| *value) {
            return Ok(true);
        }
        let keep = needles
            .iter()
            .map(|needle| needle.len())
            .max()
            .unwrap_or(1)
            - 1;
        let start = window.len().saturating_sub(keep);
        carry = window[start..].to_vec();
    }
    Ok(found.iter().all(|value| *value))
}

impl SherpaTranscriber {
    pub fn new(
        folder: &Path,
        threads: i32,
        family: OnnxFamily,
        language: Option<String>,
    ) -> Result<Self> {
        let tokens_path = folder.join("vocab.txt");
        if family != OnnxFamily::Qwen3Asr && !tokens_path.exists() {
            bail!("vocab.txt not found in {}", folder.display());
        }
        let tokens = tokens_path.display().to_string();

        let model_config = match family {
            OnnxFamily::Qwen3Asr => {
                let conv_frontend = folder.join("conv_frontend.onnx");
                let encoder = folder.join("encoder.onnx");
                let decoder = folder.join("decoder.onnx");
                if !conv_frontend.exists() || !encoder.exists() || !decoder.exists() {
                    bail!(
                        "Qwen3-ASR model is incomplete: conv_frontend.onnx, encoder.onnx and decoder.onnx are required"
                    );
                }
                let tokenizer = folder.join("tokenizer");
                if !tokenizer.join("vocab.json").exists() {
                    bail!("Qwen3-ASR tokenizer is missing: tokenizer/vocab.json");
                }
                OfflineModelConfig {
                    qwen3_asr: OfflineQwen3ASRModelConfig {
                        conv_frontend: Some(conv_frontend.display().to_string()),
                        encoder: Some(encoder.display().to_string()),
                        decoder: Some(decoder.display().to_string()),
                        tokenizer: Some(tokenizer.display().to_string()),
                        max_total_len: 512,
                        max_new_tokens: 256,
                        temperature: 0.0,
                        top_p: 1.0,
                        seed: 42,
                        hotwords: None,
                    },
                    // The engine expects an explicitly empty token file for
                    // Qwen3-ASR; the tokenizer directory carries the vocabulary.
                    tokens: Some(String::new()),
                    num_threads: threads,
                    provider: Some("cpu".into()),
                    ..Default::default()
                }
            }
            OnnxFamily::Transducer => {
                if !folder.join("encoder.onnx").exists() || !folder.join("decoder.onnx").exists() {
                    bail!("RNN-T model is incomplete: encoder.onnx and decoder.onnx are required");
                }
                // The sherpa-onnx conversions name the last component
                // `joiner.onnx`; older exports called it `joint.onnx`.
                let joiner = if folder.join("joiner.onnx").exists() {
                    folder.join("joiner.onnx")
                } else if folder.join("joint.onnx").exists() {
                    folder.join("joint.onnx")
                } else {
                    bail!("RNN-T model is incomplete: missing joiner.onnx");
                };
                ensure_sherpa_metadata(&folder.join("encoder.onnx"))?;
                OfflineModelConfig {
                    transducer: OfflineTransducerModelConfig {
                        encoder: Some(folder.join("encoder.onnx").display().to_string()),
                        decoder: Some(folder.join("decoder.onnx").display().to_string()),
                        joiner: Some(joiner.display().to_string()),
                    },
                    tokens: Some(tokens),
                    num_threads: threads,
                    provider: Some("cpu".into()),
                    ..Default::default()
                }
            }
            OnnxFamily::NemoCtc => {
                let model = folder.join("model.onnx");
                if !model.exists() {
                    bail!("model.onnx not found in {}", folder.display());
                }
                ensure_sherpa_metadata(&model)?;
                OfflineModelConfig {
                    nemo_ctc: OfflineNemoEncDecCtcModelConfig {
                        model: Some(model.display().to_string()),
                    },
                    tokens: Some(tokens),
                    num_threads: threads,
                    provider: Some("cpu".into()),
                    ..Default::default()
                }
            }
            OnnxFamily::SenseVoice => {
                let model = folder.join("model.onnx");
                if !model.exists() {
                    bail!("model.onnx not found in {}", folder.display());
                }
                OfflineModelConfig {
                    sense_voice: OfflineSenseVoiceModelConfig {
                        model: Some(model.display().to_string()),
                        language: Some("auto".into()),
                        use_itn: true,
                    },
                    tokens: Some(tokens),
                    num_threads: threads,
                    provider: Some("cpu".into()),
                    ..Default::default()
                }
            }
            OnnxFamily::Whisper => {
                let encoder = folder.join("encoder.onnx");
                let decoder = folder.join("decoder.onnx");
                if !encoder.exists() || !decoder.exists() {
                    bail!("Whisper model is incomplete: encoder.onnx and decoder.onnx are required");
                }
                OfflineModelConfig {
                    whisper: OfflineWhisperModelConfig {
                        encoder: Some(encoder.display().to_string()),
                        decoder: Some(decoder.display().to_string()),
                        language,
                        task: Some("transcribe".into()),
                        ..Default::default()
                    },
                    tokens: Some(tokens),
                    num_threads: threads,
                    provider: Some("cpu".into()),
                    ..Default::default()
                }
            }
        };

        let config = OfflineRecognizerConfig {
            model_config,
            ..Default::default()
        };
        let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
            anyhow!(
                "sherpa-onnx could not load the model in {} (check model.onnx and vocab.txt)",
                folder.display()
            )
        })?;
        Ok(Self { recognizer })
    }
}

impl Transcriber for SherpaTranscriber {
    fn transcribe(&self, recording: Recording) -> Result<String> {
        let samples = resample_to_16k(recording.samples, recording.sample_rate);
        if samples.is_empty() {
            bail!("nothing was recorded");
        }
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(16000, &samples);
        self.recognizer.decode(&stream);
        let text = stream
            .get_result()
            .map(|result| result.text)
            .unwrap_or_default();
        Ok(collapse_spaces(&text))
    }
}

pub struct WhisperTranscriber {
    model_path: PathBuf,
    /// `None` means automatic language detection.
    language: Option<String>,
    /// Model context is reused across calls (streaming re-transcribes the
    /// growing buffer several times per recording).
    ctx: std::cell::RefCell<Option<whisper_rs::WhisperContext>>,
}

impl WhisperTranscriber {
    fn ctx(&self) -> Result<std::cell::RefMut<'_, whisper_rs::WhisperContext>> {
        let mut slot = self.ctx.borrow_mut();
        if slot.is_none() {
            let ctx = whisper_rs::WhisperContext::new_with_params(
                &self.model_path,
                {
                    // Flash attention is a context-level switch in whisper.cpp
                    // and uses the optimised attention kernel on CPU too.
                    let mut parameters = whisper_rs::WhisperContextParameters::default();
                    parameters.flash_attn(true);
                    parameters
                },
            )
            .context("failed to load whisper model")?;
            *slot = Some(ctx);
        }
        Ok(std::cell::RefMut::map(slot, |s| s.as_mut().unwrap()))
    }
}

impl Transcriber for WhisperTranscriber {
    fn transcribe(&self, recording: Recording) -> Result<String> {
        let samples = resample_to_16k(recording.samples, recording.sample_rate);
        if samples.is_empty() {
            bail!("nothing was recorded");
        }
        let ctx = self.ctx()?;
        let mut state = ctx
            .create_state()
            .context("failed to create whisper state")?;
        // Greedy decoding, but with whisper.cpp's temperature fallback left at
        // its default: without it a repetition loop has no way out. No token or
        // single-segment caps either — both truncated long dictation.
        let mut params = whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy {
            best_of: 1,
        });
        params.set_language(self.language.as_deref());
        params.set_translate(false);
        params.set_no_timestamps(true);
        params.set_single_segment(false);
        params.set_no_context(true);
        params.set_temperature_inc(0.2);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // Whisper is compute-bound: give it every core (up to eight) instead of
        // the four the old code used.
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().min(8))
            .unwrap_or(4);
        params.set_n_threads(threads as i32);
        state
            .full(params, &samples)
            .context("whisper transcription failed")?;
        let n = state.full_n_segments().max(0);
        let mut parts = Vec::new();
        for i in 0..n {
            if let Some(segment) = state.get_segment(i) {
                if let Ok(text) = segment.to_str_lossy() {
                    parts.push(text.to_string());
                }
            }
        }
        Ok(collapse_spaces(&parts.join(" ")))
    }
}

fn collapse_spaces(input: &str) -> String {
    let mut out = String::new();
    let mut prev_space = false;
    for c in input.chars() {
        let is_space = c.is_whitespace();
        if is_space {
            if !prev_space {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
        prev_space = is_space;
    }
    out.trim().to_string()
}

pub fn resolve(cfg: &Config, dirs: &AppDirs) -> Result<Box<dyn Transcriber>> {
    match cfg.backend {
        Backend::Whisper => {
            let path = match cfg.whisper_model.as_str() {
                "custom" => {
                    let file = cfg
                        .whisper_file
                        .as_ref()
                        .map(PathBuf::from)
                        .context("no custom whisper model file selected: open Settings and choose it")?;
                    file.exists().then_some(file).ok_or_else(|| {
                        anyhow!("custom whisper model file does not exist")
                    })?
                }
                name => {
                    let path = model_manager::whisper_model_path(&dirs.models, name);
                    if !path.exists() {
                        bail!(
                            "whisper model '{name}' is not downloaded yet: open Settings and download it"
                        );
                    }
                    path
                }
            };
            Ok(Box::new(WhisperTranscriber {
                model_path: path,
                language: if cfg.language == "auto" {
                    None
                } else {
                    Some(cfg.language.clone())
                },
                ctx: std::cell::RefCell::new(None),
            }))
        }
        Backend::Onnx => {
            let (folder, family) = match cfg.onnx_model.as_str() {
                "custom" => {
                    let folder = cfg
                        .onnx_folder
                        .as_ref()
                        .map(PathBuf::from)
                        .context("no custom ONNX model folder selected: open Settings and choose it")?;
                    let family = model_manager::detect_family(&folder);
                    (folder, family)
                }
                name => {
                    let Some(family) = model_manager::family_of(name) else {
                        bail!(
                            "model '{name}' is not offered by this build any more: open Settings \
                             and choose a model from the list"
                        );
                    };
                    let folder = dirs.models.join(name);
                    if !model_manager::onnx_folder_ready(&folder, family) {
                        bail!(
                            "ONNX model '{name}' is not downloaded yet: open Settings and download it"
                        );
                    }
                    (folder, family)
                }
            };
            let threads = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(4) as i32;
            let language = if cfg.language == "auto" {
                None
            } else {
                Some(cfg.language.clone())
            };
            Ok(Box::new(SherpaTranscriber::new(&folder, threads, family, language)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SherpaTranscriber, Transcriber};
    use crate::model_manager::OnnxFamily;

    fn tone() -> Vec<f32> {
        (0..16_000)
            .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 220.0 / 16_000.0).sin() * 0.1)
            .collect()
    }

    #[test]
    #[ignore = "requires VB_ONNX_MODEL to point to a downloaded model folder"]
    fn sherpa_model_smoke() {
        let folder = std::env::var("VB_ONNX_MODEL").expect("VB_ONNX_MODEL is required");
        let transcriber = SherpaTranscriber::new(std::path::Path::new(&folder), 4, OnnxFamily::NemoCtc, None).unwrap();
        transcriber
            .transcribe(super::Recording {
                samples: tone(),
                sample_rate: 16_000,
            })
            .unwrap();
    }

    #[test]
    #[ignore = "requires VB_INCOMPATIBLE_MODEL to point to a non-sherpa ONNX export"]
    fn incompatible_export_is_reported_not_fatal() {
        let folder = std::env::var("VB_INCOMPATIBLE_MODEL").expect("VB_INCOMPATIBLE_MODEL is required");
        let error = SherpaTranscriber::new(std::path::Path::new(&folder), 4, OnnxFamily::NemoCtc, None)
            .err()
            .expect("a model without sherpa metadata must be rejected");
        assert!(
            error.to_string().contains("sherpa-onnx metadata"),
            "unexpected error: {error}"
        );
    }

    #[test]
    #[ignore = "requires VB_WHISPER_MODEL to point to a downloaded ggml model"]
    fn whisper_real_model_smoke() {
        let path = std::path::PathBuf::from(
            std::env::var("VB_WHISPER_MODEL").expect("VB_WHISPER_MODEL is required"),
        );
        let dirs = crate::config::AppDirs {
            root: path.parent().unwrap().to_path_buf(),
            models: path.parent().unwrap().to_path_buf(),
        };
        let mut config = crate::config::Config::default();
        config.backend = crate::config::Backend::Whisper;
        config.whisper_model = path
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .trim_start_matches("ggml-")
            .to_string();
        let transcriber = super::resolve(&config, &dirs).unwrap();
        transcriber
            .transcribe(super::Recording {
                samples: tone(),
                sample_rate: 16_000,
            })
            .unwrap();
    }
}