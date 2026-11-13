//! The HTTP contract between the phone and this computer.
//!
//! Everything here is a pure function of a request and the stored data: the
//! router never touches a socket, which is what makes every route cheap to
//! test. Transport (TLS, tokens, the setup page) lives in `server.rs`.
//!
//! Vocabulary: a **monolog** is a named thread (formerly "group"), a
//! **message** is one spoken take. The stage of a message is the only thing the
//! phone shows, so the router refuses any request that would skip a stage.

use std::sync::Mutex;

use super::db::{Db, Message};
use super::monolog::Stage;
use crate::http::{Request, Response};

/// What the router needs from the transcription queue. Tests pass a recorder.
pub trait Engine {
    /// Audio arrived for a message; the bytes are raw PCM16 at 16 kHz.
    fn audio(&self, id: u64, samples: &[u8]);
    /// The phone says it stopped sending.
    fn finish(&self, id: u64);
    /// The user asked to try a message again.
    fn retry(&self, id: u64);
    /// The message is gone; drop whatever is running for it.
    fn cancel(&self, id: u64);
    /// Is the engine busy with the person at the computer right now? The phone
    /// says so on the card instead of pretending the queue is moving.
    fn busy(&self) -> bool {
        false
    }
    /// Has the phone already sent `finish` for this take? Late audio must be
    /// refused, unlike audio that arrives while earlier segments decode.
    fn take_closed(&self, _id: u64) -> bool {
        false
    }
}

/// Something that does nothing: used when the worker is not there and by tests
/// that only look at the stored state.
pub struct Silent;
impl Engine for Silent {
    fn audio(&self, _: u64, _: &[u8]) {}
    fn finish(&self, _: u64) {}
    fn retry(&self, _: u64) {}
    fn cancel(&self, _: u64) {}
}

pub struct Ctx<'a> {
    pub db: &'a Mutex<Db>,
    pub engine: &'a dyn Engine,
    /// One timestamp per request, so a single answer is internally consistent.
    pub now: &'a str,
}

fn json(status: u16, body: serde_json::Value) -> Response {
    Response::json(status, body)
}

fn error(status: u16, message: &str) -> Response {
    json(status, serde_json::json!({ "ok": false, "error": message }))
}

fn message_json(message: &Message, engine_busy: bool) -> serde_json::Value {
    // What the person at the computer is doing right now takes the engine away
    // from this message; the phone can only say "the engine is busy" about a
    // message that still waits for it.
    let waiting = engine_busy
        && matches!(
            message.stage,
            super::monolog::Stage::Draft
                | super::monolog::Stage::Uploading
                | super::monolog::Stage::Queued
                | super::monolog::Stage::Transcribing
        );
    serde_json::json!({
        "id": message.id,
        "monolog": message.monolog,
        "recorded": message.recorded,
        "received": message.received,
        // One word for the phone: the stage, not a pile of flags.
        "stage": message.stage,
        "text": message.text,
        "original": message.original,
        "edited": message.edited,
        "duration": message.duration,
        "segments_done": message.segments_done,
        "waiting": waiting,
    })
}

fn monolog_json(
    monolog: &super::monolog::Monolog,
    busy: bool,
    last: Option<&super::db::Message>,
) -> serde_json::Value {
    serde_json::json!({
        "id": monolog.id,
        "name": monolog.name,
        "archived": monolog.archived,
        "created": monolog.created,
        "count": monolog.count,
        // The phone list shows the last words and the date without opening the
        // monolog. Empty for a fresh monolog.
        "last": last.map(|message| message.text.clone()).unwrap_or_default(),
        "lastAt": last.map(|message| message.recorded.clone()).unwrap_or_default(),
        // Something is still happening in this monolog: recording or being
        // transcribed. The phone paints the card for it.
        "busy": busy,
    })
}

/// The API is versioned: the phone checks this and refuses to talk to a
/// computer it does not understand.
pub const API_VERSION: u32 = 3;

pub fn handle(request: &Request, ctx: &Ctx) -> Response {
    let method = request.method.as_str();
    let parts: Vec<&str> = request
        .path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();

    if parts.as_slice() == ["site", "api", "ping"] {
        return json(200, serde_json::json!({ "ok": true, "api": API_VERSION }));
    }

    match (method, parts.as_slice()) {
        (_, ["site", "api", "me", ..]) => me(ctx),
        ("GET", ["site", "api", "holiday.svg"]) => holiday_icon(),
        ("GET", ["site", "api", "monologues"]) => list_monologs(ctx, request),
        ("POST", ["site", "api", "monologues"]) => create_monolog(ctx, request),
        ("PATCH", ["site", "api", "monologues", id]) => rename_monolog(ctx, id, request),
        ("DELETE", ["site", "api", "monologues", id]) => delete_monolog(ctx, id),
        ("POST", ["site", "api", "monologues", id, "archive"]) => {
            archive_monolog(ctx, id, request)
        }
        ("GET", ["site", "api", "monologues", id, "messages"]) => list_messages(ctx, id, request),
        ("POST", ["site", "api", "messages"]) => create_message(ctx, request),
        ("GET", ["site", "api", "messages", id]) => show_message(ctx, id),
        ("PATCH", ["site", "api", "messages", id]) => edit_message(ctx, id, request),
        ("DELETE", ["site", "api", "messages", id]) => delete_message(ctx, id),
        ("POST", ["site", "api", "messages", id, "audio"]) => audio(ctx, id, request),
        ("POST", ["site", "api", "messages", id, "finish"]) => finish(ctx, id),
        ("POST", ["site", "api", "messages", id, "retry"]) => retry(ctx, id),
        _ => error(404, "no such route"),
    }
}

fn me(ctx: &Ctx) -> Response {
    let db = ctx.db.lock().unwrap();
    json(
        200,
        serde_json::json!({
            "ok": true,
            "api": API_VERSION,
            "paired": db.data().client.paired,
            "monologs": db.monolog_count(false),
            // The holiday the tray is drawing today, so the page shows the same
            // microphone. Absent on an ordinary day and when the setting is off.
            "holiday": crate::holidays::current(),
        }),
    )
}

/// The holiday microphone as SVG. The stand keeps `currentColor`, so the record
/// button's colour reaches it, and the head keeps the holiday's own colours —
/// the same drawing the tray shows.
fn holiday_icon() -> Response {
    match crate::holidays::current()
        .as_deref()
        .and_then(crate::icons::holiday_page_svg)
    {
        Some(svg) => Response::new(200, "image/svg+xml; charset=utf-8", svg.into_bytes())
            .with_header("Cache-Control", "no-store"),
        None => Response::new(404, "text/plain; charset=utf-8", b"no holiday today".to_vec()),
    }
}

fn list_monologs(ctx: &Ctx, request: &Request) -> Response {
    let archived = crate::http::query_param(&request.query, "archived").as_deref() == Some("1");
    let limit = crate::http::query_param(&request.query, "limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(1, 500);
    let offset = crate::http::query_param(&request.query, "offset")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let db = ctx.db.lock().unwrap();
    let (monologs, total) = db.monologs_page(archived, offset, limit);
    let list: Vec<serde_json::Value> = monologs
        .iter()
        .map(|monolog| {
            let busy = db.monolog_busy(monolog.id);
            let last = db.last_text(monolog.id);
            monolog_json(monolog, busy, last.as_ref())
        })
        .collect();
    json(
        200,
        serde_json::json!({
            "ok": true,
            "api": API_VERSION,
            "monologs": list,
            "total": total,
            "has_more": offset + list.len() < total,
        }),
    )
}

fn create_monolog(ctx: &Ctx, request: &Request) -> Response {
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
        return error(400, "the body is not JSON");
    };
    let name = body
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() {
        return error(400, "a monolog needs a name");
    }
    let mut db = ctx.db.lock().unwrap();
    let monolog = db.create_monolog(&name, ctx.now);
    let _ = db.save();
    json(200, serde_json::json!({ "ok": true, "monolog": monolog_json(&monolog, false, None) }))
}

fn rename_monolog(ctx: &Ctx, id: &str, request: &Request) -> Response {
    let Ok(id) = id.parse::<u64>() else {
        return error(400, "the monolog id is not a number");
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
        return error(400, "the body is not JSON");
    };
    let name = body
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() {
        return error(400, "a monolog needs a name");
    }
    let mut db = ctx.db.lock().unwrap();
    if db.monolog(id).is_none() {
        return error(404, "no such monolog");
    }
    db.rename_monolog(id, &name);
    let _ = db.save();
    json(200, serde_json::json!({ "ok": true }))
}

fn archive_monolog(ctx: &Ctx, id: &str, request: &Request) -> Response {
    let Ok(id) = id.parse::<u64>() else {
        return error(400, "the monolog id is not a number");
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
        return error(400, "the body is not JSON");
    };
    let archived = body
        .get("archived")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    let mut db = ctx.db.lock().unwrap();
    if db.monolog(id).is_none() {
        return error(404, "no such monolog");
    }
    db.archive_monolog(id, archived);
    let _ = db.save();
    json(200, serde_json::json!({ "ok": true, "archived": archived }))
}

fn delete_monolog(ctx: &Ctx, id: &str) -> Response {
    let Ok(id) = id.parse::<u64>() else {
        return error(400, "the monolog id is not a number");
    };
    let messages = {
        let mut db = ctx.db.lock().unwrap();
        let Some(messages) = db.delete_monolog(id) else {
            return error(404, "no such monolog");
        };
        messages
    };
    // The rows are gone; the audio files and any queue work belong to the
    // engine. A message whose audio cannot be removed is already unreachable.
    for message in messages {
        ctx.engine.cancel(message);
    }
    json(200, serde_json::json!({ "ok": true }))
}

fn list_messages(ctx: &Ctx, id: &str, request: &Request) -> Response {
    let Ok(id) = id.parse::<u64>() else {
        return error(400, "the monolog id is not a number");
    };
    let busy = ctx.engine.busy();
    let db = ctx.db.lock().unwrap();
    if db.monolog(id).is_none() {
        return error(404, "no such monolog");
    }
    let limit = crate::http::query_param(&request.query, "limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(50)
        .clamp(1, 500);
    let cursor = |name: &str| {
        crate::http::query_param(&request.query, name).and_then(|value| {
            let (recorded, id) = value.rsplit_once('|')?;
            Some((recorded.to_string(), id.parse::<u64>().ok()?))
        })
    };
    let before = cursor("before");
    let after = cursor("after");
    let list: Vec<serde_json::Value> = db
        .message_page(
            id,
            before.as_ref().map(|(recorded, id)| (recorded.as_str(), *id)),
            after.as_ref().map(|(recorded, id)| (recorded.as_str(), *id)),
            limit,
        )
        .iter()
        .map(|message| message_json(message, busy))
        .collect();
    json(
        200,
        serde_json::json!({
            "ok": true,
            "messages": list,
            "total": db.message_count(id),
            "has_more": list.len() >= limit,
        }),
    )
}

fn create_message(ctx: &Ctx, request: &Request) -> Response {
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
        return error(400, "the body is not JSON");
    };
    let Some(monolog) = body.get("monolog").and_then(|value| value.as_u64()) else {
        return error(400, "the message needs a monolog");
    };
    let recorded = body
        .get("recorded")
        .and_then(|value| value.as_str())
        .unwrap_or(ctx.now)
        .to_string();
    let mut db = ctx.db.lock().unwrap();
    if db.monolog(monolog).is_none() {
        return error(404, "no such monolog");
    }
    let message = db.add_message(monolog, &recorded, ctx.now);
    let _ = db.save();
    let busy = ctx.engine.busy();
    json(200, serde_json::json!({ "ok": true, "message": message_json(&message, busy) }))
}

fn show_message(ctx: &Ctx, id: &str) -> Response {
    let Ok(id) = id.parse::<u64>() else {
        return error(400, "the message id is not a number");
    };
    let db = ctx.db.lock().unwrap();
    match db.message(id) {
        Some(message) => {
            let busy = ctx.engine.busy();
            json(200, serde_json::json!({ "ok": true, "message": message_json(&message, busy) }))
        }
        None => error(404, "no such message"),
    }
}

fn audio(ctx: &Ctx, id: &str, request: &Request) -> Response {
    let Ok(id) = id.parse::<u64>() else {
        return error(400, "the message id is not a number");
    };
    if request.body.len() % 2 != 0 {
        return error(400, "audio must be PCM16, an even number of bytes");
    }
    if request.body.len() > 4 * 1024 * 1024 {
        return error(413, "the chunk is too big");
    }
    let seconds = request.body.len() as f64 / 2.0 / 16_000.0;
    {
        let mut db = ctx.db.lock().unwrap();
        let Some(message) = db.message(id) else {
            return error(404, "no such message");
        };
        // A take that is still open may keep growing even when the queue has
        // already started decoding its earlier segments (the stage is then
        // `transcribing`). Refusing those chunks used to split one recording
        // into several messages and lose the tail.
        if !matches!(
            message.stage,
            Stage::Draft | Stage::Uploading | Stage::Queued | Stage::Transcribing
        ) || message.closed
        {
            return error(409, "this message is not receiving audio");
        }
        db.add_seconds(id, seconds);
        let _ = db.save();
    }
    ctx.engine.audio(id, &request.body);
    json(200, serde_json::json!({ "ok": true, "seconds": seconds }))
}

fn finish(ctx: &Ctx, id: &str) -> Response {
    let Ok(id) = id.parse::<u64>() else {
        return error(400, "the message id is not a number");
    };
    {
        let mut db = ctx.db.lock().unwrap();
        let Some(message) = db.message(id) else {
            return error(404, "no such message");
        };
        // Finishing twice is harmless; finishing a draft with no audio is not:
        // there would be nothing to transcribe and the message would wait for
        // ever. Such a take is closed as "nothing to decode" right here.
        if message.stage == Stage::Draft {
            db.close_take(id);
            db.set_stage(id, Stage::Failed);
            let _ = db.save();
            return json(200, serde_json::json!({ "ok": true, "stage": "failed" }));
        }
        match message.stage {
            Stage::Uploading => {
                db.close_take(id);
                if !db.set_stage(id, Stage::Queued) {
                    return error(409, "this message cannot be closed");
                }
            }
            // The queue may already be transcribing the earlier segments; the
            // take is still open and this `finish` is what ends it.
            Stage::Queued | Stage::Transcribing => db.close_take(id),
            _ => {
                return json(200, serde_json::json!({ "ok": true, "stage": message.stage.as_storage() }));
            }
        }
        let _ = db.save();
    }
    ctx.engine.finish(id);
    json(200, serde_json::json!({ "ok": true, "stage": "queued" }))
}

fn retry(ctx: &Ctx, id: &str) -> Response {
    let Ok(id) = id.parse::<u64>() else {
        return error(400, "the message id is not a number");
    };
    {
        let mut db = ctx.db.lock().unwrap();
        let Some(message) = db.message(id) else {
            return error(404, "no such message");
        };
        // Only a message that did not become text may be tried again.
        if !matches!(message.stage, Stage::Failed | Stage::Silent | Stage::NoModel) {
            return error(409, "this message does not need another try");
        }
        if !db.set_stage(id, Stage::Queued) {
            return error(409, "this message cannot be queued");
        }
        let _ = db.save();
    }
    ctx.engine.retry(id);
    json(200, serde_json::json!({ "ok": true, "stage": "queued" }))
}

fn edit_message(ctx: &Ctx, id: &str, request: &Request) -> Response {
    let Ok(id) = id.parse::<u64>() else {
        return error(400, "the message id is not a number");
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
        return error(400, "the body is not JSON");
    };
    let Some(text) = body.get("text").and_then(|value| value.as_str()) else {
        return error(400, "the edit needs a text");
    };
    let mut db = ctx.db.lock().unwrap();
    let Some(message) = db.message(id) else {
        return error(404, "no such message");
    };
    // The phone edits its own copy; the computer only accepts an edit once the
    // take has a text to edit.
    if message.stage != Stage::Done {
        return error(409, "this message has no text to edit yet");
    }
    db.set_edited(id, text, ctx.now);
    let _ = db.save();
    json(200, serde_json::json!({ "ok": true }))
}

fn delete_message(ctx: &Ctx, id: &str) -> Response {
    let Ok(id) = id.parse::<u64>() else {
        return error(400, "the message id is not a number");
    };
    {
        let mut db = ctx.db.lock().unwrap();
        if db.message(id).is_none() {
            return error(404, "no such message");
        }
        db.delete(id);
        let _ = db.save();
    }
    ctx.engine.cancel(id);
    json(200, serde_json::json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Recorder {
        audio: AtomicUsize,
        finish: AtomicUsize,
        retry: AtomicUsize,
        cancel: AtomicUsize,
        busy: std::sync::atomic::AtomicBool,
    }

    impl Engine for Recorder {
        fn busy(&self) -> bool {
            self.busy.load(Ordering::SeqCst)
        }

        fn audio(&self, _: u64, _: &[u8]) {
            self.audio.fetch_add(1, Ordering::SeqCst);
        }
        fn finish(&self, _: u64) {
            self.finish.fetch_add(1, Ordering::SeqCst);
        }
        fn retry(&self, _: u64) {
            self.retry.fetch_add(1, Ordering::SeqCst);
        }
        fn cancel(&self, _: u64) {
            self.cancel.fetch_add(1, Ordering::SeqCst);
        }
    }

    const NOW: &str = "2026-11-12T15:00:00+05:00";

    fn setup() -> (Mutex<Db>, Recorder) {
        // Tests run in parallel in one process: each needs its own directory.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let case = NEXT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("vb-api-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut db = Db::open(&root).unwrap();
        db.create_monolog("Работа", NOW);
        db.save().unwrap();
        (Mutex::new(db), Recorder::default())
    }

    fn ask(db: &Mutex<Db>, engine: &dyn Engine, method: &str, path: &str, body: &[u8]) -> serde_json::Value {
        let request = Request {
            method: method.to_string(),
            path: path.to_string(),
            query: String::new(),
            headers: Vec::new(),
            body: body.to_vec(),
        };
        let response = handle(
            &request,
            &Ctx {
                db,
                engine,
                now: NOW,
            },
        );
        serde_json::from_slice(&response.body).unwrap_or_else(|_| {
            serde_json::json!({ "ok": false, "status": response.status })
        })
    }

    #[test]
    fn the_list_marks_a_monolog_with_open_work() {
        let (db, engine) = setup();
        let created = ask(&db, &engine, "POST", "/site/api/messages", br#"{"monolog":1}"#);
        let id = created["message"]["id"].as_u64().unwrap();
        let listed = ask(&db, &engine, "GET", "/site/api/monologues", b"");
        assert_eq!(listed["monologs"][0]["busy"], true, "a draft is work");
        ask(&db, &engine, "POST", &format!("/site/api/messages/{id}/finish"), b"");
        let after = ask(&db, &engine, "GET", "/site/api/monologues", b"");
        assert_eq!(after["monologs"][0]["busy"], false, "nothing is open any more");
    }

    #[test]
    fn the_archive_lists_only_archived_monologs() {
        let (db, engine) = setup();
        {
            let mut db = db.lock().unwrap();
            db.create_monolog("Архив", NOW);
            db.save().unwrap();
            let ids: Vec<u64> = db.monologs(true).iter().map(|monolog| monolog.id).collect();
            db.archive_monolog(ids[1], true);
            db.save().unwrap();
        }
        let request = Request {
            method: "GET".to_string(),
            path: "site/api/monologues".to_string(),
            query: "archived=1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let response = handle(
            &request,
            &Ctx {
                db: &db,
                engine: &engine,
                now: NOW,
            },
        );
        let answer: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        let names: Vec<&str> = answer["monologs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|monolog| monolog["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["Архив"], "the archive view must not list active monologs");
    }

    fn status_of(db: &Mutex<Db>, engine: &dyn Engine, method: &str, path: &str, body: &[u8]) -> u16 {
        let request = Request {
            method: method.to_string(),
            path: path.to_string(),
            query: String::new(),
            headers: Vec::new(),
            body: body.to_vec(),
        };
        handle(
            &request,
            &Ctx {
                db,
                engine,
                now: NOW,
            },
        )
        .status
    }

    #[test]
    fn a_monolog_is_created_listed_and_renamed() {
        let (db, engine) = setup();
        assert_eq!(ask(&db, &engine, "GET", "/site/api/monologues", b"")["monologs"][0]["name"], "Работа");
        let created = ask(&db, &engine, "POST", "/site/api/monologues", r#"{"name":"  Дом  "}"#.as_bytes());
        assert_eq!(created["monolog"]["name"], "Дом");
        assert_eq!(created["monolog"]["count"], 0);
        let renamed = ask(&db, &engine, "PATCH", "/site/api/monologues/2", r#"{"name":"Дом2"}"#.as_bytes());
        assert_eq!(renamed["ok"], true);
        let listed = ask(&db, &engine, "GET", "/site/api/monologues", b"");
        let names: Vec<&str> = listed["monologs"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|monolog| monolog["name"].as_str())
            .collect();
        assert!(names.contains(&"Работа"), "the first monolog is listed");
        assert!(names.contains(&"Дом2"), "the renamed monolog is listed");
        assert_eq!(listed["total"], 2);
    }

    #[test]
    fn a_monolog_without_a_name_is_refused() {
        let (db, engine) = setup();
        assert_eq!(ask(&db, &engine, "POST", "/site/api/monologues", br#"{"name":"   "}"#)["ok"], false);
        assert_eq!(status_of(&db, &engine, "POST", "/site/api/monologues", b"not json"), 400);
    }

    #[test]
    fn a_take_goes_from_audio_to_queued() {
        let (db, engine) = setup();
        let created = ask(&db, &engine, "POST", "/site/api/messages", br#"{"monolog":1,"recorded":"2026-11-12T14:59:00+05:00"}"#);
        let id = created["message"]["id"].as_u64().expect("id");
        assert_eq!(created["message"]["stage"], "draft");

        let chunk = vec![0u8; 32_000]; // one second of 16 kHz PCM16
        let sent = ask(&db, &engine, "POST", &format!("/site/api/messages/{id}/audio"), &chunk);
        assert_eq!(sent["seconds"], 1.0);
        assert_eq!(engine.audio.load(Ordering::SeqCst), 1);

        let closed = ask(&db, &engine, "POST", &format!("/site/api/messages/{id}/finish"), b"");
        assert_eq!(closed["stage"], "queued");
        assert_eq!(engine.finish.load(Ordering::SeqCst), 1);
        assert_eq!(ask(&db, &engine, "GET", &format!("/site/api/messages/{id}"), b"")["message"]["stage"], "queued");
    }

    #[test]
    fn audio_is_refused_once_the_take_is_closed() {
        let (db, engine) = setup();
        let created = ask(&db, &engine, "POST", "/site/api/messages", br#"{"monolog":1}"#);
        let id = created["message"]["id"].as_u64().unwrap();
        ask(&db, &engine, "POST", &format!("/site/api/messages/{id}/audio"), &vec![0u8; 3200]);
        ask(&db, &engine, "POST", &format!("/site/api/messages/{id}/finish"), b"");
        let late = ask(&db, &engine, "POST", &format!("/site/api/messages/{id}/audio"), &vec![0u8; 3200]);
        assert_eq!(late["ok"], false);
        assert_eq!(status_of(&db, &engine, "POST", &format!("/site/api/messages/{id}/audio"), &vec![0u8; 3200]), 409);
    }

    #[test]
    fn audio_keeps_arriving_while_earlier_segments_are_transcribed() {
        let (db, engine) = setup();
        let created = ask(&db, &engine, "POST", "/site/api/messages", br#"{"monolog":1}"#);
        let id = created["message"]["id"].as_u64().unwrap();
        ask(&db, &engine, "POST", &format!("/site/api/messages/{id}/audio"), &vec![0u8; 3200]);
        {
            // The queue started decoding the earlier segment while the phone is
            // still recording: the message is `transcribing`, but the take is
            // open. Its next chunk must not be refused.
            let mut db = db.lock().unwrap();
            assert!(db.set_stage(id, Stage::Queued));
            assert!(db.set_stage(id, Stage::Transcribing));
            db.save().unwrap();
        }
        let late = ask(&db, &engine, "POST", &format!("/site/api/messages/{id}/audio"), &vec![0u8; 3200]);
        assert_eq!(late["ok"], true, "audio after partial text is still part of the take");
        ask(&db, &engine, "POST", &format!("/site/api/messages/{id}/finish"), b"");
        let after = ask(&db, &engine, "POST", &format!("/site/api/messages/{id}/audio"), &vec![0u8; 3200]);
        assert_eq!(after["ok"], false, "after finish the take is closed for good");
    }

    #[test]
    fn a_take_with_no_audio_is_closed_at_once() {
        let (db, engine) = setup();
        let created = ask(&db, &engine, "POST", "/site/api/messages", br#"{"monolog":1}"#);
        let id = created["message"]["id"].as_u64().unwrap();
        let closed = ask(&db, &engine, "POST", &format!("/site/api/messages/{id}/finish"), b"");
        // Nothing to transcribe: no queue entry, no waiting, no engine call.
        assert_eq!(closed["stage"], "failed");
        assert_eq!(engine.finish.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn only_a_failed_take_can_be_retried() {
        let (db, engine) = setup();
        let created = ask(&db, &engine, "POST", "/site/api/messages", br#"{"monolog":1}"#);
        let id = created["message"]["id"].as_u64().unwrap();
        // A draft has nothing to retry.
        assert_eq!(status_of(&db, &engine, "POST", &format!("/site/api/messages/{id}/retry"), b""), 409);
        ask(&db, &engine, "POST", &format!("/site/api/messages/{id}/finish"), b"");
        let retried = ask(&db, &engine, "POST", &format!("/site/api/messages/{id}/retry"), b"");
        assert_eq!(retried["stage"], "queued");
        assert_eq!(engine.retry.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_busy_engine_is_named_on_the_message() {
        let (db, engine) = setup();
        let created = ask(&db, &engine, "POST", "/site/api/messages", br#"{"monolog":1}"#);
        let id = created["message"]["id"].as_u64().unwrap();
        assert_eq!(created["message"]["waiting"], false);
        engine.busy.store(true, Ordering::SeqCst);
        let listed = ask(&db, &engine, "GET", "/site/api/monologues/1/messages", b"");
        assert_eq!(listed["messages"][0]["waiting"], true);
        // A closed take is not waiting for the engine, whatever it is doing.
        ask(&db, &engine, "POST", &format!("/site/api/messages/{id}/finish"), b"");
        let shown = ask(&db, &engine, "GET", &format!("/site/api/messages/{id}"), b"");
        assert_eq!(shown["message"]["stage"], "failed");
        assert_eq!(shown["message"]["waiting"], false);
    }

    #[test]
    fn only_a_finished_take_can_be_edited() {
        let (db, engine) = setup();
        let created = ask(&db, &engine, "POST", "/site/api/messages", br#"{"monolog":1}"#);
        let id = created["message"]["id"].as_u64().unwrap();
        assert_eq!(status_of(&db, &engine, "PATCH", &format!("/site/api/messages/{id}"), br#"{"text":"x"}"#), 409);
    }

    #[test]
    fn deleting_hides_the_take_and_tells_the_engine() {
        let (db, engine) = setup();
        let created = ask(&db, &engine, "POST", "/site/api/messages", br#"{"monolog":1}"#);
        let id = created["message"]["id"].as_u64().unwrap();
        assert_eq!(ask(&db, &engine, "DELETE", &format!("/site/api/messages/{id}"), b"")["ok"], true);
        assert_eq!(engine.cancel.load(Ordering::SeqCst), 1);
        assert!(ask(&db, &engine, "GET", "/site/api/monologues/1/messages", b"")["messages"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(status_of(&db, &engine, "DELETE", &format!("/site/api/messages/{id}"), b""), 200);
    }

    #[test]
    fn deleting_a_monolog_removes_it_and_its_messages() {
        let (db, engine) = setup();
        let first = ask(
            &db,
            &engine,
            "POST",
            "/site/api/messages",
            br#"{"monolog":1}"#,
        )["message"]["id"]
            .as_u64()
            .expect("the first message id");
        let second = ask(
            &db,
            &engine,
            "POST",
            "/site/api/messages",
            br#"{"monolog":1}"#,
        )["message"]["id"]
            .as_u64()
            .expect("the second message id");
        let before = db.lock().unwrap().store().revision("rev.site");
        let deleted = ask(&db, &engine, "DELETE", "/site/api/monologues/1", b"");
        assert_eq!(deleted["ok"], true);
        assert_eq!(
            engine.cancel.load(Ordering::SeqCst),
            2,
            "both takes lose their audio and queue work"
        );
        let after = db.lock().unwrap().store().revision("rev.site");
        assert!(
            after > before,
            "the window must be told to read the list again"
        );
        {
            let db = db.lock().unwrap();
            assert!(db.monolog(1).is_none(), "the monolog row is gone");
            assert!(db.message(first).is_none(), "the first message row is gone");
            assert!(
                db.message(second).is_none(),
                "the second message row is gone"
            );
            assert_eq!(db.message_count(1), 0);
        }
        assert_eq!(
            status_of(&db, &engine, "GET", "/site/api/monologues/1/messages", b""),
            404
        );
        assert_eq!(
            ask(&db, &engine, "GET", "/site/api/monologues", b"")["total"],
            0,
            "the list no longer has it"
        );
        assert_eq!(
            status_of(&db, &engine, "DELETE", "/site/api/monologues/1", b""),
            404,
            "a second delete finds nothing"
        );
    }

    #[test]
    fn deleting_a_monolog_needs_the_bearer_token() {
        let (db, engine) = setup();
        {
            let mut db = db.lock().unwrap();
            db.start_pairing("secret", NOW);
            db.save().unwrap();
        }
        let root = db.lock().unwrap().root().to_path_buf();
        let ca = root.join("ca.crt");
        let gate = crate::site::gate::Gate {
            db: &db,
            engine: &engine,
            static_dir: &root,
            ca: &ca,
            now: NOW,
        };
        let ask_gate = |token: Option<&str>| {
            let headers = match token {
                Some(token) => vec![("Authorization".to_string(), format!("Bearer {token}"))],
                None => Vec::new(),
            };
            let request = Request {
                method: "DELETE".to_string(),
                path: "/site/api/monologues/1".to_string(),
                query: String::new(),
                headers,
                body: Vec::new(),
            };
            crate::site::gate::answer(&request, &gate).status
        };
        assert_eq!(ask_gate(None), 401, "no token, no delete");
        assert_eq!(ask_gate(Some("wrong")), 401, "a wrong token is not a token");
        assert!(
            db.lock().unwrap().monolog(1).is_some(),
            "a refused call changed nothing"
        );
        assert_eq!(ask_gate(Some("secret")), 200, "the right token deletes it");
        assert!(db.lock().unwrap().monolog(1).is_none());
    }

    #[test]
    fn unknown_things_are_reported_as_such() {
        let (db, engine) = setup();
        assert_eq!(status_of(&db, &engine, "GET", "/site/api/messages/999", b""), 404);
        assert_eq!(status_of(&db, &engine, "GET", "/site/api/monologues/999/messages", b""), 404);
        assert_eq!(status_of(&db, &engine, "POST", "/nonsense", b""), 404);
        assert_eq!(status_of(&db, &engine, "POST", "/site/api/messages", br#"{"monolog":42}"#), 404);
    }

    #[test]
    fn a_broken_body_is_a_client_error() {
        let (db, engine) = setup();
        assert_eq!(status_of(&db, &engine, "POST", "/site/api/messages", b"oops"), 400);
        assert_eq!(status_of(&db, &engine, "POST", "/site/api/messages/abc/audio", &[0, 0]), 400);
        assert_eq!(
            status_of(&db, &engine, "POST", "/site/api/messages", br#"{"monolog":1}"#),
            200
        );
        // Odd byte count cannot be PCM16.
        assert_eq!(status_of(&db, &engine, "POST", "/site/api/messages/1/audio", &[0]), 400);
    }

    #[test]
    fn the_phone_learns_the_api_version_and_whether_it_is_paired() {
        let (db, engine) = setup();
        let ping = ask(&db, &engine, "GET", "/site/api/ping", b"");
        assert_eq!(ping["api"], API_VERSION);
        let me = ask(&db, &engine, "GET", "/site/api/me", b"");
        assert_eq!(me["paired"], false);
        assert_eq!(me["monologs"], 1);
    }
}
