//! The `--asr-worker` process: owns the recognizer so that a failing decode
//! (ONNX Runtime throws, sherpa aborts) kills this process only.

use crate::asr::{from_pcm16, read_frame, write_frame, Reply, Request};
use crate::config;
use crate::state::ModelChoice;
use crate::transcribe::{self, Transcriber};
use std::io::BufReader;
use std::time::Instant;

/// Hard ceiling for one decode, independent of the model.
///
/// The session cuts audio into ~25 s segments (30 s at most), so anything near
/// this limit means the caller forgot to segment. GigaAM's encoder aborts the
/// process on more than 200 s of audio; Whisper survives it but works in 30 s
/// windows anyway. Checking here keeps the invariant at the engine boundary
/// instead of relying on every caller.
const MAX_SEGMENT_SECONDS: f32 = 45.0;

pub fn run() -> ! {
    let dirs = config::app_dirs();
    crate::log::init_child(&dirs.root);
    crate::log::log("asr worker started");
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = std::io::stdout();
    let mut loaded: Option<(String, Box<dyn Transcriber>)> = None;
    loop {
        let (json, payload) = match read_frame(&mut reader) {
            Ok(frame) => frame,
            Err(_) => std::process::exit(0),
        };
        let began = Instant::now();
        let reply = match serde_json::from_slice::<Request>(&json) {
            Ok(request) => decode(&request, &payload, &mut loaded, &dirs),
            Err(e) => Reply {
                ok: false,
                error: format!("bad request: {e}"),
                ..Default::default()
            },
        };
        let mut reply = reply;
        reply.ms = began.elapsed().as_millis() as u64;
        if !reply.ok {
            crate::log::log(&format!("asr worker: {}", reply.error));
        }
        let bytes = serde_json::to_vec(&reply).unwrap_or_default();
        if write_frame(&mut stdout, &bytes, &[]).is_err() {
            std::process::exit(0);
        }
    }
}

fn decode(
    request: &Request,
    payload: &[u8],
    loaded: &mut Option<(String, Box<dyn Transcriber>)>,
    dirs: &config::AppDirs,
) -> Reply {
    let samples = from_pcm16(payload);
    if samples.is_empty() {
        return Reply {
            ok: false,
            error: "no audio in the request".to_string(),
            ..Default::default()
        };
    }
    let seconds = samples.len() as f32 / request.rate.max(1) as f32;
    if seconds > MAX_SEGMENT_SECONDS {
        return Reply {
            ok: false,
            error: format!(
                "audio segment is {seconds:.1} s, at most {MAX_SEGMENT_SECONDS:.0} s is accepted: \
                 the caller must cut the recording into segments"
            ),
            ..Default::default()
        };
    }
    let needs_load = match loaded {
        Some((id, _)) => id != &request.model,
        None => true,
    };
    if needs_load {
        let began = Instant::now();
        match build(&request.model, dirs) {
            Ok(transcriber) => {
                crate::log::log(&format!(
                    "asr worker: model '{}' loaded in {} ms",
                    request.model,
                    began.elapsed().as_millis()
                ));
                *loaded = Some((request.model.clone(), transcriber));
            }
            Err(e) => {
                return Reply {
                    ok: false,
                    error: format!("{e:#}"),
                    ..Default::default()
                }
            }
        }
    }
    let Some((_, transcriber)) = loaded.as_ref() else {
        return Reply {
            ok: false,
            error: "no model".to_string(),
            ..Default::default()
        };
    };
    match transcriber.transcribe(&samples, request.rate) {
        Ok(transcript) => Reply {
            ok: true,
            text: transcript.text,
            tokens: transcript.tokens,
            times: transcript.times,
            ms: 0,
            error: String::new(),
        },
        Err(e) => Reply {
            ok: false,
            error: format!("{e:#}"),
            ..Default::default()
        },
    }
}

fn build(model: &str, dirs: &config::AppDirs) -> anyhow::Result<Box<dyn Transcriber>> {
    if model.is_empty() {
        // The user's own selection, including "custom" model paths.
        let cfg = config::load_config(&dirs.root);
        return transcribe::resolve(&cfg, dirs);
    }
    if let Some(path) = model.strip_prefix("custom:") {
        let path = std::path::Path::new(path);
        if !path.exists() {
            anyhow::bail!("custom model path does not exist: {}", path.display());
        }
        return transcribe::resolve_custom(path, dirs);
    }
    let choice = ModelChoice::parse(model)
        .ok_or_else(|| anyhow::anyhow!("unknown model id '{model}'"))?;
    transcribe::resolve_choice(&choice, dirs)
}
