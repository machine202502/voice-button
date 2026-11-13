//! Starting the phone layer for real: one call brings up the database, the
//! queue, the pump thread and the HTTPS server.
//!
//! The daemon only needs `PhoneRuntime::start(...)` and `stop()`. Everything
//! else — who decodes what, when the engine is free, what happens to a message
//! left by the last run — is decided inside the layer that has tests for it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;

use super::api::Engine;
use super::cut::RealCutter;
use super::db::Db;
use super::engine::{self, AsrDecoder};
use super::hub::{Cutter, Hub};
use super::monolog::Stage;
use super::phone::PhoneServer;
use super::queue::Did;
use super::phone::static_dir as phone_static_dir;
use crate::history::{EntryState, HistoryEntry, Source};
use crate::state::Shared;

/// How often the queue is asked whether it has work.
const TICK: Duration = Duration::from_millis(150);
/// When there is nothing to do at all, look again less often.
const IDLE_TICK: Duration = Duration::from_millis(400);

/// The segmenter the live path and a retry both use.
///
/// The segment length and the sensitivity come from the app's settings, so
/// both callers build the cutter here: a retried take must be cut exactly the
/// way it was cut while it arrived.
fn cutter_for(shared: &Shared) -> Box<dyn Cutter + Send> {
    let (target, sensitivity) = {
        let data = shared.lock();
        (data.config.site_segment_seconds, data.config.vad_sensitivity)
    };
    let hard = target.max(1.0) + 5.0;
    Box::new(RealCutter::new(target, hard, sensitivity))
}

/// The bridge between the HTTP engine and the hub.
///
/// The hub only accepts audio for a take after a cutter is registered for it,
/// and it cannot make one itself: the segment length and the sensitivity come
/// from the app's settings. Without this bridge every byte the phone sends
/// would be dropped before the queue saw it.
struct LiveEngine {
    hub: Arc<Hub>,
    shared: Arc<Shared>,
    listening: Mutex<HashSet<u64>>,
}

impl LiveEngine {
    fn new(hub: Arc<Hub>, shared: Arc<Shared>) -> Self {
        Self {
            hub,
            shared,
            listening: Mutex::new(HashSet::new()),
        }
    }
}

impl Engine for LiveEngine {
    fn audio(&self, id: u64, bytes: &[u8]) {
        {
            let mut listening = self.listening.lock().unwrap();
            if !listening.contains(&id) {
                self.hub
                    .listen(id, cutter_for(&self.shared), super::hub::now_ms());
                listening.insert(id);
            }
        }
        self.hub.audio(id, bytes);
    }

    fn finish(&self, id: u64) {
        // The take is over: forget that a cutter was registered for it. An id
        // may be reused after a delete, and a stale entry here would make the
        // next take with that id send its audio to no cutter at all.
        self.listening.lock().unwrap().remove(&id);
        self.hub.finish(id);
    }

    fn retry(&self, id: u64) {
        self.hub.retry(id);
    }

    fn cancel(&self, id: u64) {
        self.listening.lock().unwrap().remove(&id);
        self.hub.cancel(id);
    }

    fn busy(&self) -> bool {
        super::engine::engine_busy(&self.shared)
    }
}

/// Tell the window whether background work is running right now.
fn set_background(shared: &Shared, running: bool) {
    let mut data = shared.lock();
    if running {
        data.background.insert(crate::state::BackgroundJob::Phone);
    } else {
        data.background.remove(&crate::state::BackgroundJob::Phone);
    }
}

pub struct PhoneRuntime {
    pub port: u16,
    server: PhoneServer,
    hub: Arc<Hub>,
    running: Arc<AtomicBool>,
    pump: Option<JoinHandle<()>>,
}

/// The phone's wall clock in the history's own format: `YYYY-MM-DD HH:MM:SS`.
///
/// The phone owns the time of its messages, so a take recorded offline an hour
/// ago is filed where it was spoken, not where it arrived. The offset is the
/// phone's business; the history keeps local wall-clock stamps like the window
/// does (`main::now_stamp`).
fn history_stamp(recorded: &str) -> String {
    recorded
        .get(..19)
        .map(|prefix| prefix.replace('T', " "))
        .unwrap_or_else(|| recorded.to_string())
}

/// Put a completed phone take into the append-only history.
///
/// The daemon owns history, so this stays the only place the phone layer writes
/// it, and it publishes the new row into the shared index for the window. The
/// message's `history_at` is the guard: `tick` can report the same completion
/// only once, but a retried or recovered take must not be filed twice anyway.
fn record_history(root: &Path, hub: &Hub, shared: &Arc<Shared>, id: u64) {
    let model = shared.lock().config.onnx_model.clone();
    let entry = {
        let mut db = hub.db.lock().unwrap();
        let Some(message) = db.message(id) else {
            return;
        };
        // Only the final text of a finished take belongs here: a silent one has
        // nothing to file and a failed one may still be retried.
        if message.stage != Stage::Done
            || message.history_at.is_some()
            || message.text.trim().is_empty()
        {
            return;
        }
        let at = history_stamp(&message.recorded);
        let entry = HistoryEntry {
            at: at.clone(),
            text: message.text.clone(),
            model,
            parts: Vec::new(),
            source: Source::Site,
            state: EntryState::Complete,
            audio: None,
            group: db.monolog(message.monolog).map(|monolog| monolog.name.clone()),
            message: Some(id),
        };
        if !db.mark_history(id, &at) {
            return;
        }
        let _ = db.save();
        entry
    };
    match crate::history::append(root, &entry) {
        Ok(row) => shared.lock().history.push_row(row),
        Err(error) => crate::log::log(&format!(
            "site: message {id} did not reach the history: {error:#}"
        )),
    }
}

impl PhoneRuntime {
    /// Bring the layer up. Binding happens here, so a busy port is an error the
    /// caller sees, not a log line nobody reads.
    pub fn start(
        root: &Path,
        port: u16,
        shared: Arc<Shared>,
        asr: Arc<crate::asr_engine::Engine>,
        static_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let db = Arc::new(Mutex::new(Db::open(root)?));
        {
            // A take left in flight by the last run is finished, not forgotten.
            db.lock().unwrap().save()?;
        }
        let cutter_shared = shared.clone();
        let hub = Arc::new(Hub::with_cutters(
            db.clone(),
            Box::new(move || cutter_for(&cutter_shared)),
        ));
        hub.resume(super::hub::now_ms());

        let static_dir = static_dir.unwrap_or_else(phone_static_dir);
        let engine = Arc::new(LiveEngine::new(hub.clone(), shared.clone()));
        let server = PhoneServer::bind(root, port, db, engine.clone(), static_dir)?;
        let bound = server.addr().port();

        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();
        let pump_hub = hub.clone();
        let pump_shared = shared;
        let history_root = root.to_path_buf();
        let pump = std::thread::spawn(move || {
            let mut decoder = AsrDecoder::new(asr, pump_shared.clone());
            while flag.load(Ordering::SeqCst) {
                // Violet means "the engine is decoding the phone's audio", not
                // "the phone has a take somewhere". While the user at the
                // computer holds the engine, the phone waits quietly.
                let working = !engine.busy() && pump_hub.has_segments();
                set_background(&pump_shared, working);
                let did = engine::step(&pump_hub, &pump_shared, &mut decoder);
                let still = !engine.busy() && pump_hub.has_segments();
                set_background(&pump_shared, still);
                if let Did::Decoded {
                    id,
                    finished: true,
                    ..
                } = &did
                {
                    record_history(&history_root, &pump_hub, &pump_shared, *id);
                }
                let pause = match did {
                    Did::Idle => IDLE_TICK,
                    _ => TICK,
                };
                std::thread::sleep(pause);
            }
            set_background(&pump_shared, false);
        });

        Ok(Self {
            port: bound,
            server,
            hub,
            running,
            pump: Some(pump),
        })
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// Stop the pump and the server. Safe to call once; the runtime is usually
    /// just dropped.
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(pump) = self.pump.take() {
            let _ = pump.join();
        }
        // Not `halt`: the port has to be free before the settings can start the
        // server again on the same one.
        self.server.wait();
    }
}

impl Drop for PhoneRuntime {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::history::History;
    use crate::site::monolog::Stage;
    use crate::site::queue::{Decoder, Outcome};
    use crate::state::Status;

    struct Says;

    impl Decoder for Says {
        fn decode(&mut self, _samples: &[f32]) -> Result<Outcome, String> {
            Ok(Outcome::Text(" раз".into()))
        }
    }

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("vb-runtime-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn shared_with(status: Status) -> Arc<Shared> {
        let shared = Arc::new(Shared::new(Config::default(), History::from_entries(Vec::new())));
        shared.lock().status = status;
        shared
    }

    fn shared() -> Arc<Shared> {
        shared_with(Status::NoModel)
    }

    fn worker() -> Arc<crate::asr_engine::Engine> {
        Arc::new(crate::asr_engine::Engine::new(
            std::env::temp_dir().join("vb-no-such-worker.exe"),
        ))
    }

    #[test]
    fn a_take_being_recorded_is_not_violet_until_audio_is_decoded() {
        let root = temp_root("background");
        let shared = shared_with(Status::Idle);
        let mut runtime = PhoneRuntime::start(
            &root,
            0,
            shared.clone(),
            worker(),
            Some(root.join("site-app")),
        )
        .expect("start");
        let id = {
            let db = runtime.hub().db_handle();
            let mut db = db.lock().unwrap();
            let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
            let message = db.add_message(
                monolog.id,
                "2026-11-12T15:00:00+05:00",
                "2026-11-12T15:00:01+05:00",
            );
            db.save().unwrap();
            message.id
        };
        runtime
            .hub()
            .listen(id, Box::new(RealCutter::new(5.0, 10.0, 1.0)), 0);
        runtime.hub().audio(id, &vec![0u8; 32_000]);
        // The cutter exists, so this is phone work — but the queue has nothing
        // to decode yet. Violet would claim the engine is busy with it.
        assert!(runtime.hub().has_work(), "a live take is work");
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !shared.lock().background_active(),
            "a take with nothing to decode must not paint the tray violet"
        );
        runtime.hub().finish(id);
        runtime.stop();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_same_port_can_be_reused_after_the_server_stops() {
        // The window switches the phone server off and on again, or moves it to
        // another port: the old listener must release the port before the new
        // one asks for it.
        let root = temp_root("reuse");
        let mut first = PhoneRuntime::start(
            &root,
            0,
            shared(),
            worker(),
            Some(root.join("site-app")),
        )
        .expect("the first server");
        let port = first.port;
        assert_ne!(port, 0);
        first.stop();
        let mut second = PhoneRuntime::start(
            &root,
            port,
            shared(),
            worker(),
            Some(root.join("site-app")),
        )
        .expect("the port must be free after stop");
        assert_eq!(second.port, port);
        second.stop();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_port_with_a_closed_connection_can_still_be_reused() {
        // Windows holds a served port in TIME_WAIT after the app closes first.
        // Rebinding right after that used to fail with os error 10048, which
        // looked like "the phone server did not start" for no visible reason.
        let root = temp_root("reuse-busy");
        let mut first = PhoneRuntime::start(
            &root,
            0,
            shared(),
            worker(),
            Some(root.join("site-app")),
        )
        .expect("the first server");
        let port = first.port;
        let client = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
        std::thread::sleep(Duration::from_millis(150));
        first.stop();
        drop(client);
        let mut second = PhoneRuntime::start(
            &root,
            port,
            shared(),
            worker(),
            Some(root.join("site-app")),
        )
        .expect("the port must be reusable while the old connection closes");
        second.stop();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_runtime_brings_the_server_and_the_queue_up() {
        let root = temp_root("up");
        let mut runtime = PhoneRuntime::start(&root, 0, shared(), worker(), Some(root.join("site-app")))
            .expect("the layer must start");
        assert_ne!(runtime.port, 0, "a real port was chosen");
        // The queue is there and answers: nothing is waiting, nothing is stuck.
        assert!(runtime.hub().stages().is_empty(), "a fresh database has no messages");
        runtime.stop();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_leftover_take_is_settled_on_the_next_start() {
        let root = temp_root("leftover");
        {
            let mut db = Db::open(&root).unwrap();
            let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
            let message = db.add_message(
                monolog.id,
                "2026-11-12T15:00:00+05:00",
                "2026-11-12T15:00:01+05:00",
            );
            db.add_seconds(message.id, 4.0);
            db.set_progress(message.id, "успел", 1);
            db.save().unwrap();
        }
        let mut runtime = PhoneRuntime::start(
            &root,
            0,
            shared_with(Status::Idle),
            worker(),
            Some(root.join("site-app")),
        )
        .expect("start");
        // The pump has work: the leftover must leave the "in flight" stages.
        let mut settled = false;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(50));
            let stages = runtime.hub().stages();
            if stages.iter().all(|(_, stage)| !stage.is_open()) {
                settled = true;
                break;
            }
        }
        assert!(
            settled,
            "a leftover take must not stay in flight: {:?}",
            runtime.hub().stages()
        );
        let stages = runtime.hub().stages();
        assert!(
            stages.iter().any(|(_, stage)| *stage == super::super::monolog::Stage::Done),
            "the transcribed text is kept: {stages:?}"
        );
        runtime.stop();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_first_chunk_opens_the_queue_for_a_phone_take() {
        let root = temp_root("live");
        let db = Arc::new(Mutex::new(Db::open(&root).unwrap()));
        let message = {
            let mut db = db.lock().unwrap();
            let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
            db.add_message(
                monolog.id,
                "2026-11-12T15:00:00+05:00",
                "2026-11-12T15:00:01+05:00",
            )
        };
        let hub = Arc::new(Hub::new(db.clone()));
        let engine = LiveEngine::new(hub.clone(), shared_with(Status::Idle));
        // One second of a tone loud enough to count as speech to the cutter.
        let pcm: Vec<u8> = (0..16_000)
            .flat_map(|index| {
                let t = index as f32 / 16_000.0;
                let sample = (t * 220.0 * std::f32::consts::TAU).sin() * 0.4;
                ((sample * 32767.0) as i16).to_le_bytes()
            })
            .collect();
        engine.audio(message.id, &pcm);
        engine.finish(message.id);

        let mut decoder = Says;
        let did = hub.pump(crate::site::hub::now_ms(), true, false, &mut decoder);
        assert!(
            matches!(did, Did::Decoded { finished: true, .. }),
            "the chunk must reach the queue: {did:?}"
        );
        assert_eq!(
            hub.db.lock().unwrap().message(message.id).unwrap().stage,
            Stage::Done
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_finished_phone_take_reaches_the_history_once() {
        let root = temp_root("history");
        let db = Arc::new(Mutex::new(Db::open(&root).unwrap()));
        let message = {
            let mut db = db.lock().unwrap();
            let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
            db.add_message(
                monolog.id,
                "2026-11-12T15:00:00+05:00",
                "2026-11-12T15:00:01+05:00",
            )
        };
        let hub = Hub::new(db.clone());
        {
            let mut db = db.lock().unwrap();
            db.set_progress(message.id, "готово", 1);
            for step in [
                Stage::Uploading,
                Stage::Queued,
                Stage::Transcribing,
                Stage::Done,
            ] {
                assert!(db.set_stage(message.id, step), "the queue owns the stage");
            }
            db.save().unwrap();
        }
        let shared = shared_with(Status::Idle);
        record_history(&root, &hub, &shared, message.id);
        // A retry or a repeated report must not file the same take twice.
        record_history(&root, &hub, &shared, message.id);

        let history = crate::history::History::load(&root);
        assert_eq!(history.len(), 1, "one take, one entry");
        let entry: HistoryEntry = history.entry(0).unwrap();
        assert_eq!(entry.source, Source::Site, "the window says where it came from");
        assert_eq!(entry.message, Some(message.id), "it points at the phone message");
        assert_eq!(entry.text, "готово");
        assert_eq!(entry.group.as_deref(), Some("Работа"));
        assert_eq!(entry.at, "2026-11-12 15:00:00", "the phone's own time");
        assert_eq!(shared.lock().history.len(), 1, "the window index has it at once");
        let _ = std::fs::remove_dir_all(root);
    }
}
