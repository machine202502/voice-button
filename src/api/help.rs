//! `GET /api/help`: a machine-readable description of this API.
//!
//! The point is that an integration (an editor plugin, an agent, a script) can
//! discover the whole contract at runtime instead of reading the binary or
//! relying on out-of-date docs. Everything the caller needs — endpoints, body
//! shapes, the audio format, the session rules, errors and runnable examples —
//! is returned in one JSON document.

use serde_json::{json, Value};

use super::{MAX_AUDIO_CHUNK, MAX_REQUEST};

/// Version of the document shape itself, bumped when fields change meaning.
const HELP_VERSION: u32 = 1;

pub fn document(base_url: &str) -> Value {
    json!({
        "helpVersion": HELP_VERSION,
        "service": "voice-button",
        "version": env!("CARGO_PKG_VERSION"),
        "baseUrl": base_url,
        "summary": "Local speech to text. The caller captures its own microphone audio, streams it here as PCM16 and receives live and final text. The app's own microphone is not used by this API.",
        "requirements": [
            "The Voice Button app must be running.",
            "The API server must be enabled in its API tab (Host, Port).",
            "A transcription model must be downloaded and selected (see GET /api/models)."
        ],
        "auth": {
            "type": "none",
            "note": "The API trusts anyone who can reach the port. Keep Host = 127.0.0.1 unless the network is trusted."
        },
        "transport": {
            "protocol": "HTTP/1.1",
            "connection": "close (one request per TCP connection)",
            "encoding": "JSON for control endpoints, raw bytes for audio",
            "cors": {
                "allowOrigin": "*",
                "allowMethods": ["GET", "POST", "OPTIONS"],
                "allowHeaders": ["Content-Type"],
                "preflight": "OPTIONS is answered with 204 and CORS headers for every path"
            },
            "limits": {
                "requestBytes": MAX_REQUEST,
                "audioChunkBytes": MAX_AUDIO_CHUNK,
                "commandTimeoutMs": 3000
            }
        },
        "audio": {
            "format": "pcm_s16le",
            "channels": 1,
            "sampleRates": { "min": 8000, "max": 48000, "default": 16000 },
            "note": "Send the rate you actually capture in POST /api/record/start (sampleRate); the app resamples to 16 kHz internally. Recommended chunk: 200-500 ms (about 6-16 KB at 16 kHz).",
            "silence": "Empty or fully silent audio produces an empty text, not an error."
        },
        "session": {
            "rule": "One session at a time, owned by the API caller (owner = \"server\").",
            "lifecycle": "POST /api/record/start -> many POST /api/record/audio -> POST /api/record/stop -> poll GET /api/status until sessionId is null -> GET /api/record/text",
            "liveText": "GET /api/status and GET /api/record/live return a growing partial transcript while audio arrives (ONNX models re-decode the buffer about once a second; Whisper only returns the final text).",
            "userPriority": "If the user is dictating in the app, POST /api/record/start returns 409 and nothing is interrupted. The user may also stop the API session from the app: the audio received so far is still transcribed, saved to history, and reported with stoppedBy = \"user\".",
            "abandonment": "There is no idle timeout. If the caller disappears without calling stop, the session keeps recording until someone stops it in the app.",
            "sideEffects": "The text is saved to the app's history with source = \"server\" and is NOT copied to the clipboard. Start/end sound cues and the tray icon behave as for any recording."
        },
        "endpoints": endpoints(),
        "errors": {
            "shape": { "ok": false, "error": "human readable reason" },
            "codes": {
                "200": "success",
                "204": "CORS preflight, empty body",
                "400": "malformed request: bad JSON body, unknown or not downloaded model, sampleRate out of range, odd PCM length, missing session parameter",
                "404": "unknown route",
                "409": "state conflict: the app is busy with another session, the session id is not active, or the chunk is for a finished session",
                "413": "audio chunk larger than audioChunkBytes",
                "504": "the app did not answer within commandTimeoutMs (busy or shutting down)"
            }
        },
        "quickstart": [
            {
                "step": "1. Check that the service is up",
                "request": "GET /api/status",
                "expect": { "state": "idle", "sessionId": null }
            },
            {
                "step": "2. Pick a model (optional; omit to use the user's current model)",
                "request": "GET /api/models",
                "expect": [{ "id": "Onnx/gigaam-v3-rnnt-punct", "ready": true }]
            },
            {
                "step": "3. Open a session with the rate of your audio",
                "request": "POST /api/record/start",
                "body": { "model": "Onnx/gigaam-v3-rnnt-punct", "sampleRate": 16000 },
                "expect": { "ok": true, "state": "recording", "sessionId": 1, "sampleRate": 16000 }
            },
            {
                "step": "4. Stream audio chunks every ~250 ms",
                "request": "POST /api/record/audio?session=1",
                "contentType": "application/octet-stream",
                "body": "PCM16 little-endian mono samples",
                "expect": { "ok": true, "receivedMs": 250, "liveText": "..." }
            },
            {
                "step": "5. Stop and wait for the text",
                "request": "POST /api/record/stop?session=1",
                "then": "Poll GET /api/status until sessionId is null, then read GET /api/record/text."
            }
        ],
        "examples": examples(base_url),
        "integrationNotes": [
            "Polling: GET /api/status every 300-500 ms is enough; it is cheap and does not touch the model.",
            "The session id in audio/stop requests must match the one returned by start; 0 is accepted by stop as 'the current session'.",
            "If a chunk is rejected with 409, the session is over: stop capturing and read GET /api/record/text.",
            "The text of a session is available after it ends; before that, use liveText for the preview.",
            "Model ids are always '<Engine>/<model>' where Engine is 'Whisper' or 'Onnx'; only ids from GET /api/models with ready = true are accepted.",
            "The audio endpoint takes the raw body, not multipart/form-data and not a WAV container."
        ]
    })
}

fn endpoints() -> Value {
    json!([
        {
            "method": "GET",
            "path": "/api/help",
            "summary": "This document: how to talk to the server, all endpoints, audio format, session rules and examples.",
            "response": "the JSON you are reading"
        },
        {
            "method": "GET",
            "path": "/api/status",
            "summary": "Current state of the app and of the API session.",
            "response": {
                "service": "voice-button",
                "version": "0.2.6",
                "state": "idle | recording | transcribing | nomodel",
                "owner": "user | server | null",
                "sessionId": 1,
                "model": "Onnx/gigaam-v3-rnnt-punct",
                "receivedMs": 1250,
                "sampleRate": 16000,
                "liveText": "partial transcript"
            },
            "notes": "owner = \"user\" means the user is dictating in the app; wait before starting. receivedMs/sampleRate/liveText are null or empty when no API session is active. A local dictation of the user is never exposed here."
        },
        {
            "method": "GET",
            "path": "/api/models",
            "summary": "Models this build offers, with a ready flag (downloaded on disk).",
            "response": [
                { "engine": "Onnx", "model": "gigaam-v3-rnnt-punct", "id": "Onnx/gigaam-v3-rnnt-punct", "ready": true, "size": "225 MB" }
            ],
            "notes": "Use id in POST /api/record/start. ready = false means the files are missing; the server will reject that id with 400."
        },
        {
            "method": "POST",
            "path": "/api/record/start",
            "summary": "Open a streaming session. The app starts listening for uploaded audio; the local microphone is not opened.",
            "contentType": "application/json",
            "body": { "model": "Onnx/gigaam-v3-rnnt-punct", "sampleRate": 16000 },
            "bodyFields": {
                "model": "optional string '<Engine>/<id>'; omit to use the user's selected model",
                "sampleRate": "optional integer 8000..48000, default 16000"
            },
            "response": { "ok": true, "state": "recording", "sessionId": 1, "sampleRate": 16000 },
            "errors": ["400 unknown or not downloaded model", "400 sampleRate out of range", "409 the app is busy with another session", "504 no answer from the app"],
            "notes": "The session starts immediately; audio sent before the first chunk is simply empty. If the caller dies here, the session has to be stopped from the app."
        },
        {
            "method": "POST",
            "path": "/api/record/audio",
            "summary": "Append audio to the open session.",
            "query": { "session": "required integer, the sessionId from start" },
            "contentType": "application/octet-stream",
            "body": "raw PCM16 little-endian, mono, at the sampleRate declared in start",
            "response": { "ok": true, "receivedMs": 1250, "liveText": "partial transcript so far" },
            "errors": ["400 missing session parameter", "400 odd number of bytes", "413 chunk too large", "409 no active session with that id", "504 no answer from the app"],
            "notes": "Send chunks of 200-500 ms. Keep one request in flight at a time so the samples stay ordered."
        },
        {
            "method": "POST",
            "path": "/api/record/stop",
            "summary": "Stop receiving audio and transcribe the whole buffer.",
            "query": { "session": "optional integer; omit or 0 for the current session" },
            "response": { "ok": true, "state": "stopping" },
            "errors": ["409 the id is not the active session", "504 no answer from the app"],
            "notes": "Returns immediately. The final text is ready when GET /api/status reports sessionId = null; read it with GET /api/record/text. The user may also stop the session from the app."
        },
        {
            "method": "GET",
            "path": "/api/record/live",
            "summary": "Live transcript of the active API session.",
            "response": { "text": "partial transcript", "receivedMs": 1250 },
            "notes": "Empty text when no API session is active. Equivalent to liveText in /api/status."
        },
        {
            "method": "GET",
            "path": "/api/record/text",
            "summary": "Result of the last API session.",
            "response": {
                "sessionId": 1,
                "text": "final transcript",
                "model": "Onnx/gigaam-v3-rnnt-punct",
                "at": "2026-11-10 03:30:00",
                "stoppedBy": "server | user",
                "error": null
            },
            "notes": "sessionId is null until the first API session ends. stoppedBy = \"user\" means the user ended the session from the app. error is a string when transcription failed (for example, a model that cannot be loaded)."
        }
    ])
}

fn examples(base_url: &str) -> Value {
    json!([
        {
            "language": "javascript",
            "title": "Browser: capture the microphone and stream it",
            "code": format!(r#"// 1) get the microphone, 2) stream 250 ms PCM16 chunks, 3) read the text.
const BASE = "{base_url}";
const media = await navigator.mediaDevices.getUserMedia({{ audio: true }});
const ctx = new AudioContext({{ sampleRate: 16000 }});
const source = ctx.createMediaStreamSource(media);

// AudioWorklet that forwards raw Float32 frames to the main thread.
const worklet = `class Tap extends AudioWorkletProcessor {{
  process(inputs) {{ const ch = inputs[0][0]; if (ch) this.port.postMessage(ch.slice(0)); return true; }}
}}
registerProcessor('tap', Tap);`;
const url = URL.createObjectURL(new Blob([worklet], {{ type: 'application/javascript' }}));
await ctx.audioWorklet.addModule(url);
const tap = new AudioWorkletNode(ctx, 'tap');

const start = await fetch(BASE + '/api/record/start', {{
  method: 'POST', headers: {{ 'Content-Type': 'application/json' }},
  body: JSON.stringify({{ sampleRate: ctx.sampleRate }}),
}}).then((r) => r.json());

let queue = [];
tap.port.onmessage = (event) => queue.push(event.data);
const mute = ctx.createGain();
mute.gain.value = 0;                      // processing without echoing the mic back
source.connect(tap).connect(mute).connect(ctx.destination);

const sender = setInterval(async () => {{
  if (!queue.length) return;
  const frames = queue.splice(0);
  const total = frames.reduce((n, f) => n + f.length, 0);
  const pcm = new Int16Array(total);
  let i = 0;
  for (const f of frames) for (const s of f) pcm[i++] = Math.max(-1, Math.min(1, s)) * 0x7fff;
  await fetch(`${{BASE}}/api/record/audio?session=${{start.sessionId}}`, {{
    method: 'POST', headers: {{ 'Content-Type': 'application/octet-stream' }}, body: pcm.buffer,
  }});
}}, 250);

// later: clearInterval(sender); await fetch(BASE + '/api/record/stop?session=' + start.sessionId, {{ method: 'POST' }});
// then poll GET /api/status until sessionId === null and read GET /api/record/text"#)
        },
        {
            "language": "powershell",
            "title": "Script: send a WAV file's PCM16 payload",
            "code": format!(r#"# Reads 16-bit mono PCM from a .wav starting at byte 44 (standard 44-byte header),
# then streams it in 250 ms chunks. Works with any recording tool.
$base = "{base_url}"
$session = (Invoke-RestMethod "$base/api/record/start" -Method Post -ContentType 'application/json' -Body '{{"sampleRate":16000}}').sessionId
$bytes = [IO.File]::ReadAllBytes('sample.wav')
$rate = 16000
$chunk = $rate * 2 / 4          # 250 ms
for ($i = 44; $i -lt $bytes.Length; $i += $chunk) {{
    $n = [Math]::Min($chunk, $bytes.Length - $i)
    $payload = New-Object byte[] $n
    [Array]::Copy($bytes, $i, $payload, 0, $n)
    Invoke-RestMethod "$base/api/record/audio?session=$session" -Method Post `
        -ContentType 'application/octet-stream' -Body $payload | Out-Null
}}
Invoke-RestMethod "$base/api/record/stop?session=$session" -Method Post | Out-Null
do {{ Start-Sleep -Milliseconds 300; $s = Invoke-RestMethod "$base/api/status" }} while ($null -ne $s.sessionId)
(Invoke-RestMethod "$base/api/record/text").text"#)
        },
        {
            "language": "curl",
            "title": "Minimal session with a single chunk",
            "code": format!(r#"curl -s {base_url}/api/status
curl -s -X POST {base_url}/api/record/start -H 'Content-Type: application/json' -d '{{"sampleRate":16000}}'
curl -s -X POST "{base_url}/api/record/audio?session=1" -H 'Content-Type: application/octet-stream' --data-binary @chunk.pcm
curl -s -X POST "{base_url}/api/record/stop?session=1"
curl -s {base_url}/api/record/text"#)
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::document;

    #[test]
    fn the_document_lists_every_route_and_the_audio_format() {
        let doc = document("http://127.0.0.1:8931");
        assert_eq!(doc["service"], "voice-button");
        assert_eq!(doc["baseUrl"], "http://127.0.0.1:8931");
        let paths: Vec<&str> = doc["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|endpoint| endpoint["path"].as_str().unwrap())
            .collect();
        for expected in [
            "/api/help",
            "/api/status",
            "/api/models",
            "/api/record/start",
            "/api/record/audio",
            "/api/record/stop",
            "/api/record/live",
            "/api/record/text",
        ] {
            assert!(paths.contains(&expected), "help must document {expected}");
        }
        assert_eq!(doc["audio"]["format"], "pcm_s16le");
        assert_eq!(doc["audio"]["channels"], 1);
        assert_eq!(doc["session"]["rule"], "One session at a time, owned by the API caller (owner = \"server\").");
        assert!(!doc["examples"].as_array().unwrap().is_empty());
    }
}
