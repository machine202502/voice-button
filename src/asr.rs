//! Client side of the ASR worker process.
//!
//! The recognizer lives in a child process (`voice-button.exe --asr-worker`).
//! ONNX Runtime reports failures by throwing C++ exceptions, and an exception
//! that reaches Rust aborts the whole process (`fatal runtime error: Rust cannot
//! catch foreign exceptions`) — the crash this app used to suffer on long
//! recordings. With the engine in its own process, a failing decode costs one
//! restart and one segment, never the recording.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long one decode may take before the worker is considered hung. A 30 s
/// segment decodes in about a second; a minute is already pathological.
pub const DECODE_TIMEOUT: Duration = Duration::from_secs(90);
/// Restarting faster than this is a crash loop, not a recovery.
const MAX_RESTARTS: usize = 6;
const RESTART_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    /// `Onnx/gigaam-v3-rnnt-punct`, or empty for the user's current model.
    pub model: String,
    pub rate: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tokens: Vec<String>,
    #[serde(default)]
    pub times: Vec<f32>,
    #[serde(default)]
    pub ms: u64,
    #[serde(default)]
    pub error: String,
}

pub fn write_frame(writer: &mut impl Write, json: &[u8], payload: &[u8]) -> Result<()> {
    writer.write_all(&(json.len() as u32).to_le_bytes())?;
    writer.write_all(json)?;
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame(reader: &mut impl Read) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut size = [0u8; 4];
    reader.read_exact(&mut size).context("read frame header")?;
    let json_len = u32::from_le_bytes(size) as usize;
    if json_len > 1 << 20 {
        bail!("frame header is implausible: {json_len} bytes of JSON");
    }
    let mut json = vec![0u8; json_len];
    reader.read_exact(&mut json).context("read frame json")?;
    reader.read_exact(&mut size).context("read payload header")?;
    let payload_len = u32::from_le_bytes(size) as usize;
    if payload_len > 64 << 20 {
        bail!("frame payload is implausible: {payload_len} bytes");
    }
    let mut payload = vec![0u8; payload_len];
    reader.read_exact(&mut payload).context("read frame payload")?;
    Ok((json, payload))
}

pub fn pcm16(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

pub fn from_pcm16(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect()
}

pub struct AsrClient {
    exe: PathBuf,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    replies: Option<mpsc::Receiver<Result<Reply>>>,
    restarts: Vec<Instant>,
    /// Model the running worker has loaded, so a change forces a restart.
    loaded: Option<String>,
}

impl AsrClient {
    pub fn new(exe: PathBuf) -> Self {
        Self {
            exe,
            child: None,
            stdin: None,
            replies: None,
            restarts: Vec::new(),
            loaded: None,
        }
    }

    /// Number of worker restarts in the recent window.
    pub fn recent_restarts(&self) -> usize {
        let now = Instant::now();
        self.restarts
            .iter()
            .filter(|at| now.duration_since(**at) < RESTART_WINDOW)
            .count()
    }

    fn spawn(&mut self, model: &str) -> Result<()> {
        let mut child = Command::new(&self.exe)
            .arg("--asr-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("cannot start the ASR worker {}", self.exe.display()))?;
        let stdin = child.stdin.take().context("worker stdin")?;
        let stdout = child.stdout.take().context("worker stdout")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let reply = read_frame(&mut reader)
                    .map_err(|e| anyhow!("{e:#}"))
                    .and_then(|(json, _)| {
                        serde_json::from_slice::<Reply>(&json).context("decode worker reply")
                    });
                let failed = reply.is_err();
                if tx.send(reply).is_err() || failed {
                    break;
                }
            }
        });
        self.child = Some(child);
        self.stdin = Some(stdin);
        self.replies = Some(rx);
        self.loaded = Some(model.to_string());
        Ok(())
    }

    /// Drop the worker; it is respawned on the next decode.
    pub fn reset(&mut self) {
        self.replies = None;
        self.stdin = None;
        self.loaded = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub fn decode(&mut self, model: &str, samples: &[f32], rate: u32) -> Result<Reply> {
        let restart = self.child.is_none() || self.loaded.as_deref() != Some(model);
        if restart {
            self.reset();
            if self.recent_restarts() >= MAX_RESTARTS {
                bail!(
                    "the transcription engine keeps failing ({} restarts in a minute)",
                    self.recent_restarts()
                );
            }
            self.restarts.push(Instant::now());
            self.spawn(model)?;
        }
        let payload = pcm16(samples);
        let json = serde_json::to_vec(&Request {
            model: model.to_string(),
            rate,
        })?;
        let stdin = self.stdin.as_mut().context("no worker stdin")?;
        if let Err(e) = write_frame(stdin, &json, &payload) {
            self.reset();
            return Err(anyhow!("cannot send audio to the ASR worker: {e}"));
        }
        let receiver = self.replies.as_ref().context("no worker channel")?;
        match receiver.recv_timeout(DECODE_TIMEOUT) {
            Ok(Ok(reply)) => {
                if reply.ok {
                    Ok(reply)
                } else {
                    Err(anyhow!(if reply.error.is_empty() {
                        "the transcription engine reported an error".to_string()
                    } else {
                        reply.error
                    }))
                }
            }
            Ok(Err(e)) => {
                self.reset();
                Err(anyhow!("the transcription engine stopped: {e:#}"))
            }
            Err(_) => {
                self.reset();
                Err(anyhow!(
                    "the transcription engine did not answer in {} s",
                    DECODE_TIMEOUT.as_secs()
                ))
            }
        }
    }
}

impl Drop for AsrClient {
    fn drop(&mut self) {
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::{from_pcm16, pcm16, read_frame, write_frame};

    #[test]
    fn a_frame_round_trips() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, b"{\"rate\":16000}", b"payload").expect("write");
        let mut cursor = std::io::Cursor::new(buffer);
        let (json, payload) = read_frame(&mut cursor).expect("read");
        assert_eq!(json, b"{\"rate\":16000}");
        assert_eq!(payload, b"payload");
    }

    #[test]
    fn pcm_round_trips_within_one_lsb() {
        let samples = vec![0.0f32, 0.5, -0.5, 1.0, -1.0, 0.123];
        let back = from_pcm16(&pcm16(&samples));
        assert_eq!(back.len(), samples.len());
        for (a, b) in samples.iter().zip(&back) {
            assert!((a - b).abs() < 1e-4, "{a} != {b}");
        }
    }
}
