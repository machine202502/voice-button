use crate::config::Backend;
use crate::state::Shared;
use anyhow::{bail, Result};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub struct RemoteFile {
    pub rel: &'static str,
    pub url: &'static str,
}

/// Which sherpa-onnx offline configuration an ONNX model folder needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OnnxFamily {
    NemoCtc,
    Transducer,
    SenseVoice,
    Qwen3Asr,
    Whisper,
}

pub struct RemoteModel {
    pub id: &'static str,
    pub format: Backend,
    /// Approximate download size shown in the model list.
    pub size: &'static str,
    /// Offline configuration for ONNX entries; `None` for whisper.cpp models.
    pub family: Option<OnnxFamily>,
    pub files: &'static [RemoteFile],
}

pub const LARGE_V3_TURBO: RemoteModel = RemoteModel {
    id: "large-v3-turbo",
    size: "1.6 GB",
    format: Backend::Whisper,
    family: None,
    files: &[RemoteFile {
        rel: "ggml-large-v3-turbo.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
    }],
};

pub const LARGE_V3: RemoteModel = RemoteModel {
    id: "large-v3",
    size: "3.1 GB",
    format: Backend::Whisper,
    family: None,
    files: &[RemoteFile {
        rel: "ggml-large-v3.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin",
    }],
};

pub const MEDIUM: RemoteModel = RemoteModel {
    id: "medium",
    size: "1.5 GB",
    format: Backend::Whisper,
    family: None,
    files: &[RemoteFile {
        rel: "ggml-medium.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
    }],
};

pub const SMALL: RemoteModel = RemoteModel {
    id: "small",
    size: "488 MB",
    format: Backend::Whisper,
    family: None,
    files: &[RemoteFile {
        rel: "ggml-small.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
    }],
};

pub const LARGE_V3_TURBO_Q5_0: RemoteModel = RemoteModel {
    id: "large-v3-turbo-q5_0",
    size: "574 MB",
    format: Backend::Whisper,
    family: None,
    files: &[RemoteFile {
        rel: "ggml-large-v3-turbo-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
    }],
};

pub const MEDIUM_Q5_0: RemoteModel = RemoteModel {
    id: "medium-q5_0",
    size: "539 MB",
    format: Backend::Whisper,
    family: None,
    files: &[RemoteFile {
        rel: "ggml-medium-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium-q5_0.bin",
    }],
};

pub const SMALL_Q5_1: RemoteModel = RemoteModel {
    id: "small-q5_1",
    size: "190 MB",
    format: Backend::Whisper,
    family: None,
    files: &[RemoteFile {
        rel: "ggml-small-q5_1.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin",
    }],
};

/// GigaAM v3 CTC, converted for the sherpa-onnx runtime (the engine this app
/// uses). Exports made for other runtimes lack the ONNX metadata the engine
/// reads, so only these conversions are offered.
pub const GIGAAM_V3_SHERPA_INT8: RemoteModel = RemoteModel {
    id: "gigaam-v3-sherpa-int8",
    size: "225 MB",
    format: Backend::Onnx,
    family: Some(OnnxFamily::NemoCtc),
    files: &[
        RemoteFile {
            rel: "gigaam-v3-sherpa-int8/vocab.txt",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-ctc-giga-am-v3-russian-2025-12-16/resolve/main/tokens.txt",
        },
        RemoteFile {
            rel: "gigaam-v3-sherpa-int8/model.onnx",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-ctc-giga-am-v3-russian-2025-12-16/resolve/main/model.int8.onnx",
        },
    ],
};

/// Same checkpoint as the CTC entry, with punctuation and casing in the output.
pub const GIGAAM_V3_CTC_PUNCT: RemoteModel = RemoteModel {
    id: "gigaam-v3-ctc-punct",
    size: "225 MB",
    format: Backend::Onnx,
    family: Some(OnnxFamily::NemoCtc),
    files: &[
        RemoteFile {
            rel: "gigaam-v3-ctc-punct/vocab.txt",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-ctc-punct-giga-am-v3-russian-2025-12-16/resolve/main/tokens.txt",
        },
        RemoteFile {
            rel: "gigaam-v3-ctc-punct/model.onnx",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-ctc-punct-giga-am-v3-russian-2025-12-16/resolve/main/model.int8.onnx",
        },
    ],
};

pub const GIGAAM_V3_RNNT: RemoteModel = RemoteModel {
    id: "gigaam-v3-rnnt",
    size: "226 MB",
    format: Backend::Onnx,
    family: Some(OnnxFamily::Transducer),
    files: &[
        RemoteFile {
            rel: "gigaam-v3-rnnt/vocab.txt",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16/resolve/main/tokens.txt",
        },
        RemoteFile {
            rel: "gigaam-v3-rnnt/encoder.onnx",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16/resolve/main/encoder.int8.onnx",
        },
        RemoteFile {
            rel: "gigaam-v3-rnnt/decoder.onnx",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16/resolve/main/decoder.onnx",
        },
        RemoteFile {
            rel: "gigaam-v3-rnnt/joiner.onnx",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-transducer-giga-am-v3-russian-2025-12-16/resolve/main/joiner.onnx",
        },
    ],
};

/// RNN-T with punctuation and casing, converted for sherpa-onnx.
pub const GIGAAM_V3_RNNT_PUNCT: RemoteModel = RemoteModel {
    id: "gigaam-v3-rnnt-punct",
    size: "226 MB",
    format: Backend::Onnx,
    family: Some(OnnxFamily::Transducer),
    files: &[
        RemoteFile {
            rel: "gigaam-v3-rnnt-punct/vocab.txt",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16/resolve/main/tokens.txt",
        },
        RemoteFile {
            rel: "gigaam-v3-rnnt-punct/encoder.onnx",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16/resolve/main/encoder.int8.onnx",
        },
        RemoteFile {
            rel: "gigaam-v3-rnnt-punct/decoder.onnx",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16/resolve/main/decoder.onnx",
        },
        RemoteFile {
            rel: "gigaam-v3-rnnt-punct/joiner.onnx",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16/resolve/main/joiner.onnx",
        },
    ],
};

/// Multilingual Whisper converted for sherpa-onnx. Slower than whisper.cpp on
/// the same hardware, but it is the only ONNX entry that covers most languages.
pub const WHISPER_BASE_ONNX: RemoteModel = RemoteModel {
    id: "whisper-base-onnx",
    size: "180 MB",
    format: Backend::Onnx,
    family: Some(OnnxFamily::Whisper),
    files: &[
        RemoteFile {
            rel: "whisper-base-onnx/vocab.txt",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-base/resolve/main/base-tokens.txt",
        },
        RemoteFile {
            rel: "whisper-base-onnx/encoder.onnx",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-base/resolve/main/base-encoder.int8.onnx",
        },
        RemoteFile {
            rel: "whisper-base-onnx/decoder.onnx",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-base/resolve/main/base-decoder.int8.onnx",
        },
    ],
};

/// Multilingual (zh/en/ja/ko/yue) SenseVoice: small, very fast, no punctuation.
pub const SENSE_VOICE: RemoteModel = RemoteModel {
    id: "sense-voice",
    size: "230 MB",
    format: Backend::Onnx,
    family: Some(OnnxFamily::SenseVoice),
    files: &[
        RemoteFile {
            rel: "sense-voice/vocab.txt",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main/tokens.txt",
        },
        RemoteFile {
            rel: "sense-voice/model.onnx",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main/model.int8.onnx",
        },
    ],
};

/// Previous GigaAM generation, kept as an alternative to v3.
pub const GIGAAM_V2_CTC: RemoteModel = RemoteModel {
    id: "gigaam-v2-ctc",
    size: "226 MB",
    format: Backend::Onnx,
    family: Some(OnnxFamily::NemoCtc),
    files: &[
        RemoteFile {
            rel: "gigaam-v2-ctc/vocab.txt",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-ctc-giga-am-v2-russian-2025-04-19/resolve/main/tokens.txt",
        },
        RemoteFile {
            rel: "gigaam-v2-ctc/model.onnx",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-ctc-giga-am-v2-russian-2025-04-19/resolve/main/model.int8.onnx",
        },
    ],
};

/// Qwen3-ASR 0.6B converted for sherpa-onnx: multilingual, larger and slower
/// than SenseVoice, but covers many more languages.
pub const QWEN3_ASR_06B: RemoteModel = RemoteModel {
    id: "qwen3-asr-0.6b",
    size: "1.1 GB",
    format: Backend::Onnx,
    family: Some(OnnxFamily::Qwen3Asr),
    files: &[
        RemoteFile {
            rel: "qwen3-asr-0.6b/conv_frontend.onnx",
            url: "https://huggingface.co/csukuangfj2/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/resolve/main/conv_frontend.onnx",
        },
        RemoteFile {
            rel: "qwen3-asr-0.6b/encoder.onnx",
            url: "https://huggingface.co/csukuangfj2/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/resolve/main/encoder.int8.onnx",
        },
        RemoteFile {
            rel: "qwen3-asr-0.6b/decoder.onnx",
            url: "https://huggingface.co/csukuangfj2/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/resolve/main/decoder.int8.onnx",
        },
        RemoteFile {
            rel: "qwen3-asr-0.6b/tokenizer/merges.txt",
            url: "https://huggingface.co/csukuangfj2/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/resolve/main/tokenizer/merges.txt",
        },
        RemoteFile {
            rel: "qwen3-asr-0.6b/tokenizer/vocab.json",
            url: "https://huggingface.co/csukuangfj2/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/resolve/main/tokenizer/vocab.json",
        },
        RemoteFile {
            rel: "qwen3-asr-0.6b/tokenizer/tokenizer_config.json",
            url: "https://huggingface.co/csukuangfj2/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/resolve/main/tokenizer/tokenizer_config.json",
        },
    ],
};
/// Multilingual entries first, then Russian-focused ones. Whisper models come
/// first in the multilingual block because they can detect the spoken language.
/// Per-model override of the longest audio a single decode may take.
///
/// Every engine the app ships works on short utterances, so the default (25 s)
/// covers all of them: whisper.cpp splits audio into 30 s windows itself,
/// SenseVoice and GigaAM are trained on short phrases, Qwen3-ASR is limited by
/// tokens. A model that accepts less only needs a line here.
pub fn max_seconds_override(id: &str) -> Option<f32> {
    match id {
        // Whisper processes 30 s windows, so 25 s is the natural unit.
        "large-v3-turbo" | "large-v3" | "medium" | "small" | "large-v3-turbo-q5_0"
        | "medium-q5_0" | "small-q5_1" | "whisper-base-onnx" => Some(25.0),
        // GigaAM is trained on short utterances and its encoder mask covers
        // 200 s; 25 s keeps the decoding well inside both.
        "gigaam-v3-sherpa-int8" | "gigaam-v3-ctc-punct" | "gigaam-v3-rnnt"
        | "gigaam-v3-rnnt-punct" | "gigaam-v2-ctc" => Some(25.0),
        // SenseVoice expects short clips as well.
        "sense-voice" => Some(25.0),
        // Qwen3-ASR is bounded by its 256-token decoder budget; the margin is
        // deliberately conservative.
        "qwen3-asr-0.6b" => Some(20.0),
        // Custom or unknown model: the default in `transcribe` applies.
        _ => None,
    }
}

pub const MODELS: &[&RemoteModel] = &[
    &LARGE_V3_TURBO_Q5_0,
    &LARGE_V3_TURBO,
    &LARGE_V3,
    &MEDIUM_Q5_0,
    &MEDIUM,
    &SMALL_Q5_1,
    &SMALL,
    &WHISPER_BASE_ONNX,
    &SENSE_VOICE,
    &QWEN3_ASR_06B,
    &GIGAAM_V3_SHERPA_INT8,
    &GIGAAM_V3_CTC_PUNCT,
    &GIGAAM_V3_RNNT,
    &GIGAAM_V3_RNNT_PUNCT,
    &GIGAAM_V2_CTC,
];

/// Whether a model can detect the spoken language automatically (the "auto"
/// language option). Whisper models can; the other ONNX models cannot.
pub fn supports_auto(format: Backend, id: &str) -> bool {
    match format {
        Backend::Whisper => true,
        Backend::Onnx => matches!(
            find_model(id).and_then(|model| model.family),
            Some(OnnxFamily::Whisper) | Some(OnnxFamily::SenseVoice)
        ),
    }
}

pub fn find_model(id: &str) -> Option<&'static RemoteModel> {
    MODELS.iter().copied().find(|m| m.id == id)
}

pub fn family_of(id: &str) -> Option<OnnxFamily> {
    find_model(id).and_then(|model| model.family)
}

pub fn file_path(models_dir: &Path, file: &RemoteFile) -> PathBuf {
    models_dir.join(file.rel)
}

pub fn is_ready(models_dir: &Path, id: &str) -> bool {
    match find_model(id) {
        Some(model) => model
            .files
            .iter()
            .all(|f| file_path(models_dir, f).exists()),
        None => false,
    }
}

/// Guess the ONNX configuration from the files a folder contains.
pub fn detect_family(path: &Path) -> OnnxFamily {
    if path.join("joiner.onnx").exists() || path.join("joint.onnx").exists() {
        OnnxFamily::Transducer
    } else if path.join("conv_frontend.onnx").exists() {
        OnnxFamily::Qwen3Asr
    } else if path.join("model.onnx").exists() {
        // Nemo CTC and SenseVoice both ship a single model.onnx; the CTC
        // metadata check in the transcriber tells them apart in practice.
        OnnxFamily::NemoCtc
    } else {
        OnnxFamily::Whisper
    }
}
pub fn onnx_folder_ready(path: &Path, family: OnnxFamily) -> bool {
    let vocab_ok = path.join("vocab.txt").exists()
        || path.join("vocab.json").exists()
        || path.join("tokenizer").join("vocab.json").exists();
    let ready = match family {
        OnnxFamily::NemoCtc => path.join("model.onnx").exists(),
        OnnxFamily::SenseVoice => path.join("model.onnx").exists(),
        OnnxFamily::Qwen3Asr => {
            path.join("conv_frontend.onnx").exists()
                && path.join("encoder.onnx").exists()
                && path.join("decoder.onnx").exists()
        }
        OnnxFamily::Transducer => {
            let joiner = path.join("joiner.onnx").exists() || path.join("joint.onnx").exists();
            path.join("encoder.onnx").exists() && path.join("decoder.onnx").exists() && joiner
        }
        OnnxFamily::Whisper => {
            path.join("encoder.onnx").exists() && path.join("decoder.onnx").exists()
        }
    };
    vocab_ok && ready
}

/// Whether the configured model selection is usable right now (downloaded,
/// or a custom file/folder that exists).
pub fn selection_ready(models_dir: &Path, cfg: &crate::config::Config) -> bool {
    match cfg.backend {
        Backend::Whisper => match cfg.whisper_model.as_str() {
            "custom" => cfg
                .whisper_file
                .as_ref()
                .map(|f| PathBuf::from(f).exists())
                .unwrap_or(false),
            id => is_ready(models_dir, id),
        },
        Backend::Onnx => match cfg.onnx_model.as_str() {
            "custom" => cfg
                .onnx_folder
                .as_ref()
                .map(|folder| onnx_folder_ready(Path::new(folder), detect_family(Path::new(folder))))
                .unwrap_or(false),
            id => is_ready(models_dir, id),
        },
    }
}

pub fn whisper_model_path(models_dir: &Path, name: &str) -> PathBuf {
    models_dir.join(format!("ggml-{name}.bin"))
}

pub fn start_download(shared: Arc<Shared>, models_dir: PathBuf, id: String) {
    let Some(model) = find_model(&id) else {
        shared.fail_download(crate::i18n::trf(
            "Unknown model: {model}",
            &[("model", &id)],
        ));
        return;
    };
    let root = models_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| models_dir.clone());
    crate::ipc::clear_cancel(&root);
    crate::log::log(&format!("download started: {id}"));
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let mut data = shared.lock();
        data.download = Some(crate::state::DownloadProgress {
            model: id.clone(),
            file: 0,
            files: model.files.len(),
            downloaded: 0,
            total: None,
            done_bytes: 0,
            done_total: 0,
            done: false,
            error: None,
            cancel: cancel.clone(),
        });
    }
    std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            for (index, file) in model.files.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) || crate::ipc::cancel_requested(&root) {
                    bail!("cancelled");
                }
                shared.set_download_file(index);
                let resp = ureq::get(file.url).call()?;
                let total = resp
                    .header("content-length")
                    .and_then(|v| v.parse::<u64>().ok());
                shared.set_download_total(total);
                let dest = file_path(&models_dir, file);
                if let Some(parent) = dest.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let tmp = PathBuf::from(format!("{}.part", dest.display()));
                let mut reader = resp.into_reader();
                let mut out = File::create(&tmp)?;
                let mut buffer = vec![0u8; 1 << 16];
                let mut count: u64 = 0;
                let mut last: u64 = 0;
                loop {
                    if cancel.load(Ordering::Relaxed) || crate::ipc::cancel_requested(&root) {
                        drop(out);
                        let _ = std::fs::remove_file(&tmp);
                        bail!("cancelled");
                    }
                    let n = reader.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    out.write_all(&buffer[..n])?;
                    count += n as u64;
                    if count - last >= (1 << 19) {
                        last = count;
                        shared.set_download_progress(count);
                    }
                }
                shared.set_download_progress(count);
                if count == 0 {
                    drop(out);
                    let _ = std::fs::remove_file(&tmp);
                    anyhow::bail!(
                        "{}",
                        crate::i18n::trf(
                            "The downloaded file is empty: {file}",
                            &[("file", file.rel)]
                        )
                    );
                }
                drop(out);
                std::fs::rename(&tmp, &dest)?;
                shared.complete_download_file();
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                crate::log::log(&format!("download finished: {id}"));
                shared.finish_download()
            }
            Err(e) => {
                if cancel.load(Ordering::Relaxed) || crate::ipc::cancel_requested(&root) {
                    crate::log::log(&format!("download cancelled: {id}"));
                    crate::ipc::clear_cancel(&root);
                    shared.clear_download();
                } else {
                    crate::log::log(&format!("download failed ({id}): {e:#}"));
                    shared.fail_download(format!("{e:#}"));
                }
            }
        }
    });
}

