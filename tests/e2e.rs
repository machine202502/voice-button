//! End-to-end checks against a real recording and a real model.
//!
//! They are `#[ignore]`d because they need a downloaded model (hundreds of
//! megabytes) and the `voice-button.exe` binary next to the test executable:
//!
//! ```text
//! cargo test --release --test e2e -- --ignored --nocapture
//! ```
//!
//! `VB_E2E_MODEL` overrides the model id (`Onnx/gigaam-v3-rnnt-punct` by
//! default), `VB_ASR_EXE` overrides the worker binary.

use std::path::{Path, PathBuf};
use std::time::Instant;

fn asset(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/assets")
        .join(name)
}

fn model_id() -> String {
    std::env::var("VB_E2E_MODEL").unwrap_or_else(|_| "Onnx/gigaam-v3-rnnt-punct".to_string())
}

fn worker_exe() -> PathBuf {
    if let Ok(path) = std::env::var("VB_ASR_EXE") {
        return PathBuf::from(path);
    }
    let test_exe = std::env::current_exe().expect("test executable");
    // target/<profile>/deps/<test>.exe -> target/<profile>/voice-button.exe
    let profile_dir = test_exe
        .parent()
        .and_then(Path::parent)
        .expect("target profile directory");
    profile_dir.join("voice-button.exe")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("voice-button-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Decode the MP3 asset into 16 kHz mono samples.
fn load_audio() -> (Vec<f32>, u32) {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let path = asset("dictation-8min.mp3");
    let file = std::fs::File::open(&path).expect("the recording asset");
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .expect("probe the mp3");
    let mut format = probed.format;
    let track = format.default_track().expect("an audio track").clone();
    let track_id = track.id;
    let rate = track
        .codec_params
        .sample_rate
        .expect("the asset has a sample rate");
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .expect("mp3 decoder");
    let mut samples = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => panic!("decode the asset: {e}"),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count();
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        for frame in buffer.samples().chunks(channels) {
            samples.push(frame.iter().sum::<f32>() / channels as f32);
        }
    }
    (samples, rate)
}

/// Write the asset as the 16 kHz WAV a crashed session would leave behind.
fn write_recording(dir: &Path, samples: &[f32]) {
    use voice_button::audio::WavWriter;
    let mut writer = WavWriter::create(&dir.join("audio.wav"), 16_000).expect("wav");
    writer.write(samples).expect("audio");
    writer.finish().expect("finish");
}

fn normalize_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(|c| c.to_lowercase())
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

fn word_error_rate(reference: &str, hypothesis: &str) -> f64 {
    let a = normalize_words(reference);
    let b = normalize_words(hypothesis);
    if a.is_empty() {
        return if b.is_empty() { 0.0 } else { 1.0 };
    }
    // Levenshtein distance over words.
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, left) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, right) in b.iter().enumerate() {
            let cost = if left == right { 0 } else { 1 };
            let value = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
            current.push(value);
        }
        previous = current;
    }
    previous[b.len()] as f64 / a.len() as f64
}

#[test]
#[ignore = "needs a downloaded model and the app binary"]
fn the_eight_minute_recording_is_transcribed_whole() {
    let exe = worker_exe();
    assert!(
        exe.exists(),
        "the ASR worker binary is missing: {} (build it or set VB_ASR_EXE)",
        exe.display()
    );
    let (samples, rate) = load_audio();
    assert_eq!(rate, 16_000, "the asset is 16 kHz mono");
    let seconds = samples.len() as f64 / 16_000.0;
    assert!(seconds > 470.0, "the asset must stay the full recording");

    let dir = temp_dir("whole");
    write_recording(&dir, &samples);

    let client = std::sync::Arc::new(voice_button::asr_engine::Engine::new(exe));
    let began = Instant::now();
    let (text, audio_seconds, failed) =
        voice_button::session::recover(&client, &dir, &model_id(), 25.0, 1.0).expect("recovery");
    let elapsed = began.elapsed();

    let reference = std::fs::read_to_string(asset("dictation-8min.txt")).expect("reference");
    let wer = word_error_rate(&reference, &text);
    // Kept for a manual look when the threshold trips.
    std::fs::write(std::env::temp_dir().join("vb-e2e-transcript.txt"), &text).ok();
    println!(
        "8 min of audio in {:.1} s ({:.1}s of audio), {:.2} % WER, {} failed segments",
        elapsed.as_secs_f64(),
        audio_seconds,
        wer * 100.0,
        failed
    );
    assert_eq!(failed, 0, "no segment may fail");
    assert!(
        wer < 0.05,
        "the chunked transcript drifted from the reference: {:.2} % WER",
        wer * 100.0
    );
    assert!(
        elapsed.as_secs() < 120,
        "an 8 minute recording must not take minutes of CPU: {:?}",
        elapsed
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "needs a downloaded model and the app binary"]
fn a_live_session_writes_the_audio_and_reports_the_text() {
    use std::sync::{mpsc, Arc, Mutex};
    use voice_button::config::{AppDirs, Config};
    use voice_button::history::History;
    use voice_button::session::{self, SessionStart};
    use voice_button::state::{AppEvent, Interactor, ModelChoice, Shared};

    let (samples, _) = load_audio();
    let exe = worker_exe();
    let root = temp_dir("live");
    let dirs = AppDirs {
        root: root.clone(),
        models: voice_button::config::app_dirs().models,
    };
    let shared = Arc::new(Shared::new(
        Config::default(),
        History::from_entries(Vec::new()),
    ));
    let (events, received) = mpsc::channel();
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let start = SessionStart {
        id: 7,
        interactor: Interactor::user(),
        model: ModelChoice::parse(&model_id()),
        rate: 16_000,
        append: false,
        paste_target: None,
        clipboard: true,
        buffer: buffer.clone(),
    };
    let client = Arc::new(voice_button::asr_engine::Engine::new(exe));
    let handle = session::start(shared, dirs, events, client, start).expect("start the session");
    // Twelve seconds of the recording, in the 100 ms chunks the recorder hands
    // over.
    for chunk in samples[..12 * 16_000].chunks(1_600) {
        buffer.lock().unwrap().extend_from_slice(chunk);
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    handle.stop();
    let event = received
        .recv_timeout(std::time::Duration::from_secs(120))
        .expect("the session must report its result");
    match event {
        AppEvent::Transcribed { text, dir, .. } => {
            assert!(!text.trim().is_empty(), "the session produced no text");
            let dir = dir.expect("the recording directory");
            let audio = voice_button::audio::read_wav(&dir.join("audio.wav")).expect("audio.wav");
            assert!(
                audio.samples.len() > 11 * 16_000,
                "the audio file is short: {} samples",
                audio.samples.len()
            );
            println!("live session text: {text}");
        }
        other => panic!("unexpected event: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore = "needs a downloaded model and the app binary"]
fn a_dead_worker_is_replaced_and_the_next_segment_decodes() {
    let exe = worker_exe();
    assert!(exe.exists(), "the ASR worker binary is missing: {}", exe.display());
    let mut client = voice_button::asr::AsrClient::new(exe);
    let silence = vec![0.0f32; 16_000];
    let first = client.decode(&model_id(), &silence, 16_000).expect("first decode");
    assert!(first.ok);
    // Same effect as the engine aborting: the client drops the worker.
    client.reset();
    let second = client.decode(&model_id(), &silence, 16_000).expect("second decode");
    assert!(second.ok);
}

#[test]
#[ignore = "needs a downloaded model and the app binary"]
fn the_api_server_transcribes_the_recording_over_http() {
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};
    use voice_button::api::ApiServer;
    use voice_button::config::{AppDirs, Backend, Config};
    use voice_button::history::{History, Source};
    use voice_button::state::{ApiResult, AppEvent, InteractorKind, Shared, Status};

    let (samples, rate) = load_audio();
    assert_eq!(rate, 16_000);
    let exe = worker_exe();
    let root = temp_dir("api");
    let dirs = AppDirs {
        root: root.clone(),
        models: voice_button::config::app_dirs().models,
    };

    let mut cfg = Config::default();
    cfg.backend = Backend::Onnx;
    cfg.onnx_model = "gigaam-v3-rnnt-punct".to_string();
    let shared = Arc::new(Shared::new(cfg, History::from_entries(Vec::new())));
    voice_button::daemon::install(voice_button::daemon::Runtime {
        buffer: Arc::new(Mutex::new(Vec::new())),
        asr: Arc::new(voice_button::asr_engine::Engine::new(exe)),
        session: Mutex::new(None),
        file: Mutex::new(None),
    });

    let (api_tx, api_rx) = mpsc::channel();
    // The HTTP layer resolves model ids against the application directory, so it
    // must see the real models; recordings still go to the temporary directory.
    let api_root = voice_button::config::app_dirs().root;
    let server =
        ApiServer::bind("127.0.0.1", 0, shared.clone(), api_root, api_tx).expect("bind");
    let addr = server.addr();
    let base = format!("http://{addr}");

    let (tray_cmds, _tray_rx) = mpsc::channel();
    let (events_tx, events_rx) = mpsc::channel();
    // Minimal daemon: answers API commands and stores the finished session the
    // way the real daemon does.
    {
        let shared = shared.clone();
        let dirs = dirs.clone();
        let tray_cmds = tray_cmds.clone();
        std::thread::spawn(move || {
            let mut recorder = voice_button::recorder::Recorder::new();
            for request in api_rx {
                voice_button::daemon::handle_api_request(
                    request,
                    &mut recorder,
                    &shared,
                    &dirs,
                    &tray_cmds,
                    events_tx.clone(),
                );
            }
        });
    }
    {
        let shared = shared.clone();
        let dirs = dirs.clone();
        let tray_cmds = tray_cmds.clone();
        std::thread::spawn(move || {
            for event in events_rx {
                let AppEvent::Transcribed {
                    text,
                    model,
                    state,
                    dir,
                    ..
                } = event
                else {
                    continue;
                };
                let entry = voice_button::state::HistoryEntry {
                    at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                    text: text.clone(),
                    model: model.clone(),
                    parts: Vec::new(),
                    source: Source::Server,
                    state,
                    audio: None,
                    group: None,
                    message: None,
                };
                if let Ok(row) = voice_button::history::append(&dirs.root, &entry) {
                    shared.lock().history.push_row(row);
                }
                let finished = shared.lock().session.take();
                if let Some(session) = finished {
                    shared.lock().api_result = Some(ApiResult {
                        session: session.id,
                        text,
                        model,
                        at: entry.at.clone(),
                        stopped_by: InteractorKind::Server,
                        error: None,
                    });
                }
                if let Some(dir) = dir {
                    let _ = std::fs::remove_dir_all(dir);
                }
                voice_button::daemon::set_status(&shared, &tray_cmds, &dirs, Status::Idle);
            }
        });
    }

    fn json(url: &str) -> serde_json::Value {
        let body = ureq::get(url)
            .call()
            .unwrap_or_else(|e| panic!("GET {url}: {e}"))
            .into_string()
            .expect("body");
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("GET {url}: {e}: {body}"))
    }
    fn post(url: &str, body: &[u8]) -> serde_json::Value {
        let answer = ureq::post(url)
            .set("content-type", "application/octet-stream")
            .send_bytes(body)
            .unwrap_or_else(|e| panic!("POST {url}: {e}"))
            .into_string()
            .expect("body");
        serde_json::from_str(&answer).unwrap_or_else(|e| panic!("POST {url}: {e}: {answer}"))
    }

    let help = json(&format!("{base}/api/help"));
    assert_eq!(help["helpVersion"], 1, "the help document changed shape");
    let models = json(&format!("{base}/api/models"));
    assert!(
        models.as_array().map(|list| !list.is_empty()).unwrap_or(false),
        "the models list is empty"
    );

    let start = post(
        &format!("{base}/api/record/start"),
        format!(r#"{{"model":"{}","sampleRate":16000}}"#, model_id()).as_bytes(),
    );
    let session = start["sessionId"].as_u64().expect("a session id");
    assert_eq!(start["state"], "recording");

    // Stream the whole recording in five-second pieces, like a page would.
    let pcm = voice_button::asr::pcm16(&samples);
    for piece in pcm.chunks(5 * 16_000 * 2) {
        let answer = post(
            &format!("{base}/api/record/audio?session={session}"),
            piece,
        );
        assert_eq!(answer["ok"], true, "a chunk was rejected: {answer}");
    }
    let stopped = post(&format!("{base}/api/record/stop?session={session}"), b"");
    assert_eq!(stopped["ok"], true);

    let started = Instant::now();
    loop {
        let status = json(&format!("{base}/api/status"));
        if status["state"] == "idle" {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(240),
            "the session did not finish: {status}"
        );
        std::thread::sleep(Duration::from_millis(400));
    }

    let text = json(&format!("{base}/api/record/text"));
    let text = text["text"].as_str().unwrap_or_default().to_string();
    let reference = std::fs::read_to_string(asset("dictation-8min.txt")).expect("reference");
    let wer = word_error_rate(&reference, &text);
    println!(
        "API transcript: {} chars, {:.2} % WER, {:.1} s",
        text.chars().count(),
        wer * 100.0,
        started.elapsed().as_secs_f64()
    );
    assert!(!text.trim().is_empty(), "the API returned no text");
    assert!(
        wer < 0.05,
        "the API transcript drifted from the reference: {:.2} % WER",
        wer * 100.0
    );

    // The result is also in the history, attributed to the server.
    let mut found = false;
    // Take the count first: a guard in the loop header would be held for the
    // whole loop and the next lock would deadlock.
    let count = shared.lock().history.len();
    for index in 0..count {
        let row = shared.lock().history.row(index).expect("row").clone();
        if row.source() == Source::Server && !row.preview().is_empty() {
            found = true;
        }
    }
    assert!(found, "the API session did not reach the history");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore = "needs the app binary"]
fn the_worker_refuses_audio_longer_than_one_segment() {
    // The invariant lives at the engine boundary: a caller that forgets to cut
    // the recording must get an error, not a crash of the engine.
    let mut client = voice_button::asr::AsrClient::new(worker_exe());
    let long = vec![0.0f32; 60 * 16_000];
    let error = client
        .decode(&model_id(), &long, 16_000)
        .expect_err("a 60 s decode must be refused");
    let message = format!("{error:#}");
    assert!(
        message.contains("at most") && message.contains("segments"),
        "unexpected error: {message}"
    );
    // A normal segment on the same worker still works.
    let short = vec![0.0f32; 5 * 16_000];
    assert!(client.decode(&model_id(), &short, 16_000).expect("short decode").ok);
}

#[test]
#[ignore = "needs a downloaded model and the app binary"]
fn an_imported_file_is_transcribed_without_copying_it() {
    use std::sync::{mpsc, Arc};
    use std::time::Duration;
    use voice_button::config::{AppDirs, Backend, Config};
    use voice_button::history::{History, Source};
    use voice_button::state::{AppEvent, Shared};

    let exe = worker_exe();
    let root = temp_dir("file");
    let dirs = AppDirs {
        root: root.clone(),
        models: voice_button::config::app_dirs().models,
    };
    let mut cfg = Config::default();
    cfg.backend = Backend::Onnx;
    cfg.onnx_model = "gigaam-v3-rnnt-punct".to_string();
    let shared = Arc::new(Shared::new(cfg, History::from_entries(Vec::new())));
    let (events, received) = mpsc::channel();
    let client = Arc::new(voice_button::asr_engine::Engine::new(exe));
    let path = asset("dictation-8min.mp3");
    let handle = voice_button::session::start_file(shared, dirs.clone(), events, client, path.clone())
        .expect("start the file transcription");

    let began = std::time::Instant::now();
    let event = received
        .recv_timeout(Duration::from_secs(240))
        .expect("the file session must report its result");
    let AppEvent::Transcribed {
        text,
        source,
        audio,
        state,
        dir: recording_dir,
        ..
    } = event
    else {
        panic!("unexpected event");
    };
    let reference = std::fs::read_to_string(asset("dictation-8min.txt")).expect("reference");
    let wer = word_error_rate(&reference, &text);
    println!(
        "imported file: {} chars, {:.2} % WER, {:.1} s",
        text.chars().count(),
        wer * 100.0,
        began.elapsed().as_secs_f64()
    );
    assert_eq!(source, Source::File);
    assert_eq!(audio.as_deref(), Some(path.to_string_lossy().as_ref()));
    assert!(recording_dir.is_none(), "nothing may be written into recordings/");
    assert!(!state.is_partial() || wer < 0.05);
    assert!(wer < 0.05, "the file transcript drifted: {:.2} % WER", wer * 100.0);
    let copied = std::fs::read_dir(root.join("recordings"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(copied, 0, "the imported file must not be copied");
    handle.cancel();
    let _ = std::fs::remove_dir_all(&root);
}
