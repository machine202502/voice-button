//! Local Voice Button API server.
//!
//! A small JSON + PCM API over HTTP on the configured host and port. The
//! caller (a page, a script, another service) captures its own sound and sends
//! it here; the app transcribes that audio instead of its local microphone and
//! returns live and final text.
//!
//! Endpoints:
//!
//! - `GET  /api/status` — session state, live text, received audio length.
//! - `GET  /api/models` — models this build offers (with `ready`).
//! - `POST /api/record/start` — open a streaming session (`model`, `sampleRate`).
//! - `POST /api/record/audio?session=N` — append PCM16 LE mono samples.
//! - `POST /api/record/stop` — finish the session and transcribe the whole buffer.
//! - `GET  /api/record/live` — live transcription of the session.
//! - `GET  /api/record/text` — transcription of the last API session.
//!
//! The test page is a separate Node project in `api-demo/`; nothing is embedded
//! in the binary.

use crate::config::Backend;
use crate::model_manager;
use crate::state::{self, ModelChoice, Shared};
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

mod help;

/// Port used when the user enables the server without picking one.
pub const DEFAULT_API_PORT: u16 = 8931;

/// Bind address used when the configured host is empty.
pub const DEFAULT_API_HOST: &str = "127.0.0.1";

/// Header block plus a JSON body are small; an audio chunk is not.
const MAX_REQUEST: usize = 1024 * 1024;
/// ~16 s of 16 kHz PCM16 in one request; the demo sends ~250 ms.
const MAX_AUDIO_CHUNK: usize = 512 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);

/// What the API asks the daemon to do. The daemon owns the transcription
/// session, so the HTTP thread never touches the model.
pub enum ApiAction {
    StreamStart {
        model: Option<ModelChoice>,
        sample_rate: u32,
    },
    StreamChunk {
        session: u64,
        samples: Vec<f32>,
    },
    StreamStop {
        session: u64,
    },
}

pub struct ApiRequest {
    pub action: ApiAction,
    pub reply: mpsc::Sender<ApiResponse>,
}

/// The daemon's answer to one API command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiResponse {
    StreamStarted { session: u64 },
    ChunkAccepted { received_ms: u64, live: String },
    StreamStopped,
    Rejected(String),
}

pub struct ApiServer {
    addr: SocketAddr,
    running: Arc<AtomicBool>,
}

impl ApiServer {
    /// Bind the listener (synchronously, so a busy port is reported to the
    /// caller) and start the accept loop.
    pub fn bind(
        host: &str,
        port: u16,
        shared: Arc<Shared>,
        root: PathBuf,
        commands: mpsc::Sender<ApiRequest>,
    ) -> std::io::Result<Self> {
        let host = if host.trim().is_empty() {
            DEFAULT_API_HOST
        } else if host.trim().eq_ignore_ascii_case("localhost") {
            // Bind the IPv4 loopback explicitly: a client asking for
            // 127.0.0.1 must reach the same socket.
            DEFAULT_API_HOST
        } else {
            host.trim()
        };
        let listener = TcpListener::bind((host, port))?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let base_url = format!("http://{}:{}", display_host(addr.ip()), addr.port());
        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();
        std::thread::spawn(move || acceptor(listener, flag, shared, root, commands, base_url));
        crate::log::log(&format!("api server listening on {addr}"));
        Ok(Self { addr, running })
    }

    /// Address actually bound (port 0 resolves to the port the OS picked).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

impl Drop for ApiServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn acceptor(
    listener: TcpListener,
    running: Arc<AtomicBool>,
    shared: Arc<Shared>,
    root: PathBuf,
    commands: mpsc::Sender<ApiRequest>,
    base_url: String,
) {
    while running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                let shared = shared.clone();
                let root = root.clone();
                let commands = commands.clone();
                let base_url = base_url.clone();
                std::thread::spawn(move || serve(stream, shared, root, commands, base_url));
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                crate::log::log(&format!("api server accept failed: {e}"));
                break;
            }
        }
    }
}

/// Bind address as a client should use it: a wildcard bind is not a URL.
fn display_host(ip: IpAddr) -> String {
    if ip.is_unspecified() {
        "127.0.0.1".to_string()
    } else {
        ip.to_string()
    }
}

struct Request {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
}

struct Response {
    status: u16,
    content_type: &'static str,
    body: String,
}

fn serve(
    mut stream: TcpStream,
    shared: Arc<Shared>,
    root: PathBuf,
    commands: mpsc::Sender<ApiRequest>,
    base_url: String,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    let response = route(&request, &shared, &root, &commands, &base_url);
    let _ = write_response(&mut stream, &response);
}

/// Read one HTTP request (headers + Content-Length body). One request per
/// connection: the response closes it.
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        if let Some(pos) = find_bytes(&buf, b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > 16 * 1024 {
            return None;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target.to_string(), String::new()),
    };
    let content_length = lines
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            if key.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    if content_length > MAX_REQUEST {
        return None;
    }
    let body_start = header_end + 4;
    while buf.len() < body_start + content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = buf[body_start..body_start + content_length].to_vec();
    Some(Request {
        method,
        path,
        query,
        body,
    })
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn write_response(stream: &mut TcpStream, response: &Response) -> std::io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "OK",
    };
    // CORS is what lets the demo page (served by its own Node server on another
    // port) call the API with fetch().
    let head = format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Type: {}\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        response.status,
        reason,
        response.content_type,
        response.body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(response.body.as_bytes())?;
    stream.flush()
}

fn json_response(status: u16, value: serde_json::Value) -> Response {
    Response {
        status,
        content_type: "application/json",
        body: serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string()),
    }
}

fn error_response(status: u16, message: impl Into<String>) -> Response {
    json_response(
        status,
        serde_json::json!({ "ok": false, "error": message.into() }),
    )
}

fn route(
    request: &Request,
    shared: &Shared,
    root: &Path,
    commands: &mpsc::Sender<ApiRequest>,
    base_url: &str,
) -> Response {
    match (request.method.as_str(), request.path.as_str()) {
        // Preflight for the demo page (a different origin).
        ("OPTIONS", _) => Response {
            status: 204,
            content_type: "text/plain",
            body: String::new(),
        },
        // Self-documenting contract for integrations and agents.
        ("GET", "/api/help") => json_response(200, help::document(base_url)),
        ("GET", "/api/status") => json_response(200, status_json(shared)),
        ("GET", "/api/models") => json_response(200, models_json(root)),
        ("GET", "/api/record/live") => json_response(200, live_json(shared)),
        ("GET", "/api/record/text") => json_response(200, text_json(shared)),
        ("POST", "/api/record/start") => start_stream(request, root, commands),
        ("POST", "/api/record/audio") => audio_chunk(request, commands),
        ("POST", "/api/record/stop") => stop_stream(request, commands),
        _ => json_response(
            404,
            serde_json::json!({
                "error": "not found",
                "path": request.path,
                "hint": "GET /api/help describes this API; the demo page lives in api-demo/",
            }),
        ),
    }
}

fn selected_model(cfg: &crate::config::Config) -> ModelChoice {
    let id = match cfg.backend {
        Backend::Whisper => cfg.whisper_model.clone(),
        Backend::Onnx => cfg.onnx_model.clone(),
    };
    ModelChoice {
        backend: cfg.backend,
        id,
    }
}

fn status_json(shared: &Shared) -> serde_json::Value {
    let data = shared.lock();
    let state_name = match data.status {
        state::Status::Idle => "idle",
        state::Status::Recording => "recording",
        state::Status::Transcribing => "transcribing",
        state::Status::NoModel => "nomodel",
    };
    let session = data.session.as_ref();
    let stream = data.stream.as_ref();
    let model = session
        .and_then(|s| s.model_label())
        .unwrap_or_else(|| selected_model(&data.config).api_id());
    let received_ms = stream.map(|s| {
        s.received_samples
            .saturating_mul(1000)
            .saturating_div(s.sample_rate.max(1) as u64)
    });
    // Only the stream's own preview is exposed; a local dictation of the user
    // never reaches the caller.
    let live_text = stream.map(|s| s.live_text.clone()).unwrap_or_default();
    serde_json::json!({
        "service": "voice-button",
        "version": env!("CARGO_PKG_VERSION"),
        "state": state_name,
        "owner": session.map(|s| s.owner.kind.label()),
        "sessionId": session.map(|s| s.id),
        "model": model,
        "receivedMs": received_ms,
        "sampleRate": stream.map(|s| s.sample_rate),
        "liveText": live_text,
    })
}

fn models_json(root: &Path) -> serde_json::Value {
    let models_dir = root.join("models");
    let entries: Vec<serde_json::Value> = model_manager::MODELS
        .iter()
        .map(|m| {
            let engine = m.format.label();
            serde_json::json!({
                "engine": engine,
                "model": m.id,
                "id": format!("{engine}/{}", m.id),
                "ready": model_manager::is_ready(&models_dir, m.id),
                "size": m.size,
            })
        })
        .collect();
    serde_json::Value::Array(entries)
}

fn model_ready(root: &Path, choice: &ModelChoice) -> bool {
    let known = model_manager::find_model(&choice.id)
        .map(|known| known.format == choice.backend)
        .unwrap_or(false);
    known && model_manager::is_ready(&root.join("models"), &choice.id)
}

fn live_json(shared: &Shared) -> serde_json::Value {
    let data = shared.lock();
    match data.stream.as_ref() {
        Some(stream) => serde_json::json!({
            "text": stream.live_text,
            "receivedMs": stream.received_samples.saturating_mul(1000).saturating_div(stream.sample_rate.max(1) as u64),
        }),
        None => serde_json::json!({ "text": "", "receivedMs": serde_json::Value::Null }),
    }
}

fn text_json(shared: &Shared) -> serde_json::Value {
    let data = shared.lock();
    match &data.api_result {
        Some(result) => serde_json::json!({
            "sessionId": result.session,
            "text": result.text,
            "model": result.model,
            "at": result.at,
            "stoppedBy": result.stopped_by.label(),
            "error": result.error,
        }),
        None => serde_json::json!({
            "sessionId": serde_json::Value::Null,
            "text": "",
            "model": serde_json::Value::Null,
            "at": serde_json::Value::Null,
            "stoppedBy": serde_json::Value::Null,
            "error": serde_json::Value::Null,
        }),
    }
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

fn start_stream(request: &Request, root: &Path, commands: &mpsc::Sender<ApiRequest>) -> Response {
    let body = if request.body.is_empty() {
        serde_json::Value::Null
    } else {
        match serde_json::from_slice::<serde_json::Value>(&request.body) {
            Ok(value) => value,
            Err(_) => return error_response(400, "request body is not valid JSON"),
        }
    };
    let model = match body.get("model") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(text)) if text.trim().is_empty() => None,
        Some(serde_json::Value::String(text)) => {
            let Some(choice) = ModelChoice::parse(text) else {
                return error_response(400, "model must look like 'Onnx/<id>' or 'Whisper/<id>'");
            };
            if !model_ready(root, &choice) {
                return error_response(
                    400,
                    format!("model '{}' is not downloaded", choice.api_id()),
                );
            }
            Some(choice)
        }
        Some(_) => return error_response(400, "model must be a string"),
    };
    let sample_rate = match body.get("sampleRate") {
        None | Some(serde_json::Value::Null) => 16_000,
        Some(value) => match value.as_u64() {
            Some(rate) if (8_000..=48_000).contains(&rate) => rate as u32,
            _ => return error_response(400, "sampleRate must be between 8000 and 48000"),
        },
    };
    match send_command(
        commands,
        ApiAction::StreamStart {
            model,
            sample_rate,
        },
    ) {
        Ok(ApiResponse::StreamStarted { session }) => json_response(
            200,
            serde_json::json!({ "ok": true, "state": "recording", "sessionId": session, "sampleRate": sample_rate }),
        ),
        Ok(ApiResponse::Rejected(message)) => error_response(409, message),
        Ok(_) => error_response(409, "unexpected daemon answer"),
        Err(()) => error_response(504, "the app did not answer in time"),
    }
}

fn audio_chunk(request: &Request, commands: &mpsc::Sender<ApiRequest>) -> Response {
    if request.body.is_empty() {
        return error_response(400, "empty audio chunk");
    }
    if request.body.len() > MAX_AUDIO_CHUNK {
        return error_response(413, "audio chunk is too large");
    }
    if request.body.len() % 2 != 0 {
        return error_response(400, "audio must be little-endian PCM16");
    }
    let Some(session) = query_param(&request.query, "session").and_then(|s| s.parse::<u64>().ok())
    else {
        return error_response(400, "session query parameter is required");
    };
    let samples: Vec<f32> = request
        .body
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0)
        .collect();
    match send_command(commands, ApiAction::StreamChunk { session, samples }) {
        Ok(ApiResponse::ChunkAccepted { received_ms, live }) => json_response(
            200,
            serde_json::json!({ "ok": true, "receivedMs": received_ms, "liveText": live }),
        ),
        Ok(ApiResponse::Rejected(message)) => error_response(409, message),
        Ok(_) => error_response(409, "unexpected daemon answer"),
        Err(()) => error_response(504, "the app did not answer in time"),
    }
}

fn stop_stream(request: &Request, commands: &mpsc::Sender<ApiRequest>) -> Response {
    let session = query_param(&request.query, "session").and_then(|s| s.parse::<u64>().ok());
    match send_command(
        commands,
        ApiAction::StreamStop {
            session: session.unwrap_or(0),
        },
    ) {
        Ok(ApiResponse::StreamStopped) => {
            json_response(200, serde_json::json!({ "ok": true, "state": "stopping" }))
        }
        Ok(ApiResponse::Rejected(message)) => error_response(409, message),
        Ok(_) => error_response(409, "unexpected daemon answer"),
        Err(()) => error_response(504, "the app did not answer in time"),
    }
}

/// Hand a command to the daemon and wait for its decision.
fn send_command(
    commands: &mpsc::Sender<ApiRequest>,
    action: ApiAction,
) -> Result<ApiResponse, ()> {
    let (reply, responses) = mpsc::channel();
    commands.send(ApiRequest { action, reply }).map_err(|_| ())?;
    responses.recv_timeout(COMMAND_TIMEOUT).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::history::History;
    use crate::state::{ApiResult, Interactor, InteractorKind, Session, Status, StreamSession};

    fn shared() -> Arc<Shared> {
        Arc::new(Shared::new(
            Config::default(),
            History::from_entries(Vec::new()),
        ))
    }

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "voice-button-api-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(root.join("models"));
        root
    }

    fn start(shared: &Arc<Shared>, root: &Path, commands: mpsc::Sender<ApiRequest>) -> ApiServer {
        ApiServer::bind("127.0.0.1", 0, shared.clone(), root.to_path_buf(), commands)
            .expect("bind the test server")
    }

    /// Answers every command with the same canned response.
    fn answering(response: ApiResponse) -> mpsc::Sender<ApiRequest> {
        let (commands, rx) = mpsc::channel::<ApiRequest>();
        std::thread::spawn(move || {
            while let Ok(request) = rx.recv() {
                let _ = request.reply.send(response.clone());
            }
        });
        commands
    }

    /// One request over a fresh connection (the server closes after a reply).
    fn rpc_bytes(
        server: &ApiServer,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> (u16, String, String) {
        let mut stream = TcpStream::connect(server.addr()).expect("connect");
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        head.extend_from_slice(body);
        stream.write_all(&head).unwrap();
        stream.flush().unwrap();
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        let (header_end, content_length) = loop {
            let n = stream.read(&mut chunk).expect("read");
            assert!(n > 0, "server closed the connection early");
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = find_bytes(&buf, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        if key.trim().eq_ignore_ascii_case("content-length") {
                            value.trim().parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                if buf.len() >= pos + 4 + length {
                    break (pos, length);
                }
            }
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(0);
        let body = String::from_utf8_lossy(&buf[header_end + 4..header_end + 4 + content_length])
            .to_string();
        (status, head, body)
    }

    fn rpc(server: &ApiServer, method: &str, path: &str, body: Option<&str>) -> (u16, String, String) {
        rpc_bytes(server, method, path, body.unwrap_or("").as_bytes())
    }

    fn pcm16(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn models_are_listed_with_their_engine() {
        let (commands, _rx) = mpsc::channel();
        let server = start(&shared(), &temp_root("models"), commands);
        let (status, head, body) = rpc(&server, "GET", "/api/models", None);
        assert_eq!(status, 200);
        assert!(head.contains("Access-Control-Allow-Origin: *"));
        let models: serde_json::Value = serde_json::from_str(&body).unwrap();
        let first = models.as_array().unwrap().first().unwrap();
        assert_eq!(
            first["id"],
            format!(
                "{}/{}",
                first["engine"].as_str().unwrap(),
                first["model"].as_str().unwrap()
            )
        );
        assert!(first.get("ready").is_some());
    }

    #[test]
    fn start_rejects_an_unknown_model_and_a_silly_sample_rate_locally() {
        let (commands, _rx) = mpsc::channel();
        let server = start(&shared(), &temp_root("start-bad"), commands);
        let (status, _head, body) = rpc(
            &server,
            "POST",
            "/api/record/start",
            Some(r#"{"model":"Onnx/not-in-the-catalog"}"#),
        );
        assert_eq!(status, 400);
        assert!(body.contains("not downloaded"));

        let (status, _head, body) = rpc(
            &server,
            "POST",
            "/api/record/start",
            Some(r#"{"sampleRate":4000}"#),
        );
        assert_eq!(status, 400);
        assert!(body.contains("sampleRate"));
    }

    #[test]
    fn starting_a_stream_returns_the_session_id() {
        let server = start(
            &shared(),
            &temp_root("start-ok"),
            answering(ApiResponse::StreamStarted { session: 7 }),
        );
        let (status, _head, body) = rpc(
            &server,
            "POST",
            "/api/record/start",
            Some(r#"{"sampleRate":16000}"#),
        );
        assert_eq!(status, 200);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["sessionId"], 7);
        assert_eq!(value["state"], "recording");
    }

    #[test]
    fn audio_needs_a_session_and_whole_pcm16_samples() {
        let server = start(
            &shared(),
            &temp_root("audio-bad"),
            answering(ApiResponse::ChunkAccepted {
                received_ms: 0,
                live: String::new(),
            }),
        );
        let (status, _head, _body) = rpc_bytes(&server, "POST", "/api/record/audio", &pcm16(&[1, 2]));
        assert_eq!(status, 400);

        let (status, _head, _body) = rpc_bytes(
            &server,
            "POST",
            "/api/record/audio?session=3",
            &[1, 2, 3],
        );
        assert_eq!(status, 400);
    }

    #[test]
    fn an_audio_chunk_is_forwarded_and_acknowledged() {
        let server = start(
            &shared(),
            &temp_root("audio-ok"),
            answering(ApiResponse::ChunkAccepted {
                received_ms: 250,
                live: "привет".to_string(),
            }),
        );
        let (status, _head, body) = rpc_bytes(
            &server,
            "POST",
            "/api/record/audio?session=3",
            &pcm16(&[100, -100, 200, -200]),
        );
        assert_eq!(status, 200);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["receivedMs"], 250);
        assert_eq!(value["liveText"], "привет");
    }

    #[test]
    fn status_reports_the_stream_length_and_live_text() {
        let shared = shared();
        let root = temp_root("status");
        let (commands, _rx) = mpsc::channel();
        {
            let mut data = shared.lock();
            data.status = Status::Recording;
            data.session = Some(Session {
                id: 5,
                owner: Interactor::server(),
                model: None,
                stopped_by: None,
            });
            data.stream = Some(StreamSession {
                id: 5,
                model: None,
                sample_rate: 16_000,
                buffer: Arc::new(std::sync::Mutex::new(Vec::new())),
                received_samples: 32_000,
                live_text: "живой текст".to_string(),
                stop: Arc::new(AtomicBool::new(false)),
            });
        }
        let server = start(&shared, &root, commands);
        let (status, _head, body) = rpc(&server, "GET", "/api/status", None);
        assert_eq!(status, 200);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["state"], "recording");
        assert_eq!(value["owner"], "server");
        assert_eq!(value["receivedMs"], 2000);
        assert_eq!(value["liveText"], "живой текст");

        let (_status, _head, body) = rpc(&server, "GET", "/api/record/live", None);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["text"], "живой текст");
    }

    #[test]
    fn text_returns_only_the_last_api_result() {
        let shared = shared();
        let root = temp_root("text");
        let (commands, _rx) = mpsc::channel();
        shared.lock().api_result = Some(ApiResult {
            session: 4,
            text: "серверный текст".to_string(),
            model: "Onnx/gigaam-v3-rnnt".to_string(),
            at: "2026-11-10 03:00:00".to_string(),
            stopped_by: InteractorKind::Server,
            error: None,
        });
        let server = start(&shared, &root, commands);
        let (status, _head, body) = rpc(&server, "GET", "/api/record/text", None);
        assert_eq!(status, 200);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["sessionId"], 4);
        assert_eq!(value["text"], "серверный текст");
        assert_eq!(value["stoppedBy"], "server");
    }

    #[test]
    fn cors_preflight_is_answered_without_a_body() {
        let (commands, _rx) = mpsc::channel();
        let server = start(&shared(), &temp_root("options"), commands);
        let (status, head, body) = rpc(&server, "OPTIONS", "/api/record/start", None);
        assert_eq!(status, 204);
        assert!(body.is_empty());
        assert!(head.contains("Access-Control-Allow-Methods: GET, POST, OPTIONS"));
    }

    #[test]
    fn the_page_is_not_embedded_and_unknown_routes_are_json() {
        let (commands, _rx) = mpsc::channel();
        let server = start(&shared(), &temp_root("roots"), commands);
        let (status, head, body) = rpc(&server, "GET", "/", None);
        assert_eq!(status, 404);
        assert!(head.contains("application/json"));
        assert!(body.contains("api-demo"));
    }

    #[test]
    fn help_documents_only_routes_that_really_exist() {
        let server = start(
            &shared(),
            &temp_root("help"),
            answering(ApiResponse::Rejected("test".to_string())),
        );
        let (status, head, body) = rpc(&server, "GET", "/api/help", None);
        assert_eq!(status, 200);
        assert!(head.contains("application/json"));
        let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(doc["service"], "voice-button");
        assert_eq!(doc["helpVersion"], 1);
        assert!(doc["baseUrl"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:"));
        let endpoints = doc["endpoints"].as_array().unwrap();
        assert!(!endpoints.is_empty());
        // The documentation must not drift from the router: every endpoint it
        // advertises has to be handled by this server.
        for endpoint in endpoints {
            let method = endpoint["method"].as_str().unwrap();
            let path = endpoint["path"].as_str().unwrap();
            let (status, _head, _body) = rpc(&server, method, path, None);
            assert_ne!(status, 404, "{method} {path} is documented but not routed");
        }
    }

    #[test]
    fn stopping_without_an_active_stream_is_reported_as_stopped() {
        let server = start(
            &shared(),
            &temp_root("stop"),
            answering(ApiResponse::StreamStopped),
        );
        let (status, _head, body) = rpc(&server, "POST", "/api/record/stop", Some("{}"));
        assert_eq!(status, 200);
        assert!(body.contains("stopping"));
    }
}
