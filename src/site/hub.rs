//! The glue between the three halves of the phone layer.
//!
//! `api` speaks HTTP, `queue` decides what to decode, `db` remembers. None of
//! them knows about the others; this module is the only place that does. It is
//! also the only place that turns raw PCM into segments, which is why the
//! segmenter is a trait here: tests replace it with one that never guesses.
//!
//! The rule that matters: a message's stage in the database is written by the
//! queue and by nobody else, so the phone can never see a state the engine has
//! not reached.
//!
//! The raw audio of a take is appended to `site/audio/{id}.pcm` while it
//! arrives. That file is what makes a retry possible: the phone does not have
//! to send a finished take again, the hub re-cuts what it already has and puts
//! the segments back into the queue.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Arc, Mutex};

use super::api::Engine;
use super::db::Db;
use super::monolog::Stage;
use super::queue::{Did, Queue, Segment, Sink};

/// Splits a stream of audio into pieces worth decoding.
pub trait Cutter {
    fn push(&mut self, samples: &[f32]) -> Vec<Segment>;
    /// Whatever is left when the phone stops.
    fn flush(&mut self) -> Option<Segment>;
}

/// Makes a cutter for a retried take.
///
/// The live path gets its cutter from the caller (`listen`) because one take
/// owns one cutter. A retry has no live cutter any more and has to build one
/// from the app's settings, so the runtime passes this factory in.
pub type CutterFactory = Box<dyn Fn() -> Box<dyn Cutter + Send> + Send + Sync>;

pub struct Hub {
    pub db: Arc<Mutex<Db>>,
    pub queue: Mutex<Queue>,
    /// One cutter per message that is still receiving audio.
    cutters: Mutex<HashMap<u64, Box<dyn Cutter + Send>>>,
    /// `None` when the hub was built without the app's settings: a retry then
    /// falls back to the old empty-job behaviour instead of panicking.
    cutter_factory: Option<CutterFactory>,
}

impl Hub {
    pub fn new(db: Arc<Mutex<Db>>) -> Self {
        Self::build(db, None)
    }

    /// A hub that can re-cut persisted audio with the app's real segmenter.
    pub fn with_cutters(db: Arc<Mutex<Db>>, factory: CutterFactory) -> Self {
        Self::build(db, Some(factory))
    }

    fn build(db: Arc<Mutex<Db>>, cutter_factory: Option<CutterFactory>) -> Self {
        Self {
            db,
            queue: Mutex::new(Queue::default()),
            cutters: Mutex::new(HashMap::new()),
            cutter_factory,
        }
    }

    /// A message that already exists in the database starts receiving audio.
    ///
    /// The database lock is released before the queue is touched. Holding both
    /// here was a real deadlock: the pump holds the queue while the sink writes
    /// to the database, so "first chunk of a new take arrives during a decode"
    /// froze the whole app (violet icon forever, tray dead, no way out).
    pub fn listen(&self, id: u64, cutter: Box<dyn Cutter + Send>, now: u64) {
        let opened = {
            let mut db = self.db.lock().unwrap();
            let found = db
                .message(id)
                .map(|message| (message.segments_done, message.text.clone()));
            let _ = db.save();
            found
        };
        if let Some((done, text)) = opened {
            self.queue.lock().unwrap().open(id, done, &text, now);
        }
        self.cutters.lock().unwrap().insert(id, cutter);
    }

    /// Run one step of the queue. `now` is milliseconds from any fixed point.
    pub fn pump(
        &self,
        now: u64,
        model_ready: bool,
        engine_busy: bool,
        decoder: &mut dyn super::queue::Decoder,
    ) -> Did {
        let mut queue = self.queue.lock().unwrap();
        let mut sink = DbSink { db: &self.db };
        queue.tick(now, model_ready, engine_busy, decoder, &mut sink)
    }

    /// Pick up what a previous run left in flight.
    ///
    /// A take that was still uploading has its audio in `site/audio`, so it is
    /// replayed through a fresh cutter and finishes with its whole text. A take
    /// from before the audio was kept, or one whose file is gone, is closed as
    /// before: the queue finalizes the text already transcribed (or declares
    /// silence) instead of leaving a message that says "transcribing" for ever.
    pub fn resume(&self, now: u64) {
        let leftovers = {
            let db = self.db.lock().unwrap();
            db.unfinished()
        };
        for id in leftovers {
            let (done, text) = {
                let db = self.db.lock().unwrap();
                match db.message(id) {
                    Some(message) => (message.segments_done, message.text.clone()),
                    None => continue,
                }
            };
            if self.replay_audio(id, done, &text, now) {
                crate::log::log(&format!("site: message {id} replayed from its audio"));
                continue;
            }
            {
                let mut queue = self.queue.lock().unwrap();
                queue.open(id, done, &text, now);
                queue.close(id, now);
            }
            crate::log::log(&format!("site: message {id} picked up from the last run"));
        }
    }

    /// Append the raw PCM16 to the message's file before it is converted.
    ///
    /// A failed write costs the ability to retry, never the incoming take: the
    /// caller keeps going, and the error reaches the log.
    fn keep_audio(&self, id: u64, bytes: &[u8]) {
        let path = {
            let db = self.db.lock().unwrap();
            Db::audio_path(db.root(), id)
        };
        if let Some(dir) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(dir) {
                crate::log::log(&format!(
                    "site: cannot create the audio directory for message {id}: {error}"
                ));
                return;
            }
        }
        let written = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(bytes));
        if let Err(error) = written {
            crate::log::log(&format!(
                "site: cannot keep the audio of message {id}: {error}"
            ));
        }
    }

    /// Re-cut the persisted audio and hand every segment to a fresh job.
    ///
    /// False means "nothing to replay": the file is missing or empty, or this
    /// hub has no cutter factory. The caller then keeps the old behaviour.
    fn replay_audio(&self, id: u64, done: u32, text: &str, now: u64) -> bool {
        let path = {
            let db = self.db.lock().unwrap();
            Db::audio_path(db.root(), id)
        };
        let audio = match std::fs::read(&path) {
            Ok(audio) => audio,
            Err(error) => {
                crate::log::log(&format!(
                    "site: message {id} has no audio to retry: {error}"
                ));
                return false;
            }
        };
        if audio.is_empty() {
            crate::log::log(&format!("site: message {id} has an empty audio file"));
            return false;
        }
        let Some(factory) = self.cutter_factory.as_ref() else {
            crate::log::log(&format!(
                "site: message {id} cannot be replayed without a cutter"
            ));
            return false;
        };
        let mut cutter = factory();
        let mut segments = cutter.push(&pcm16(&audio));
        if let Some(tail) = cutter.flush() {
            segments.push(tail);
        }
        // The queue owns the stage: walk to `queued` through the sink instead
        // of writing it behind the queue's back.
        DbSink { db: &self.db }.advance(id, Stage::Queued);
        let mut queue = self.queue.lock().unwrap();
        queue.open(id, done, text, now);
        for segment in segments {
            queue.push(id, segment, now);
        }
        // The take is over: there will be no more audio and no `finish` call.
        queue.close(id, now);
        true
    }

    /// Is there phone work right now: a take still arriving, or audio the
    /// queue has not finished with? The tray icon turns violet while this is
    /// true, and no sound is played for it.
    pub fn has_work(&self) -> bool {
        if !self.cutters.lock().unwrap().is_empty() {
            return true;
        }
        self.queue.lock().unwrap().busy()
    }

    /// Audio is ready for the engine right now: only then is the tray violet.
    pub fn has_segments(&self) -> bool {
        self.queue.lock().unwrap().has_segments()
    }

    /// The phone already closed this take; late audio must be refused.
    pub fn take_closed(&self, id: u64) -> bool {
        self.queue.lock().unwrap().take_closed(id)
    }

    /// Stages the database currently knows, for the interface.
    /// A handle on the same database, for the server that serves it.
    pub fn db_handle(&self) -> Arc<Mutex<Db>> {
        self.db.clone()
    }

    pub fn stages(&self) -> Vec<(u64, Stage)> {
        self.db.lock().unwrap().stages()
    }
}

impl Engine for Hub {
    fn take_closed(&self, id: u64) -> bool {
        Hub::take_closed(self, id)
    }

    fn audio(&self, id: u64, bytes: &[u8]) {
        // Keep the bytes first: whatever happens below, the take stays
        // retryable.
        self.keep_audio(id, bytes);
        let samples = pcm16(bytes);
        {
            // The API counts the seconds; the first byte of audio only moves a
            // draft out of `draft`, because a draft may not jump to `queued`.
            let mut db = self.db.lock().unwrap();
            let Some(message) = db.message(id) else {
                return;
            };
            if message.stage == Stage::Draft {
                let _ = db.set_stage(id, Stage::Uploading);
                let _ = db.save();
            }
        }
        let mut cutters = self.cutters.lock().unwrap();
        let Some(cutter) = cutters.get_mut(&id) else {
            return;
        };
        let segments = cutter.push(&samples);
        drop(cutters);
        let mut queue = self.queue.lock().unwrap();
        for segment in segments {
            queue.push(id, segment, now_ms());
        }
    }

    fn finish(&self, id: u64) {
        let tail = self
            .cutters
            .lock()
            .unwrap()
            .remove(&id)
            .and_then(|mut cutter| cutter.flush());
        {
            let mut db = self.db.lock().unwrap();
            let _ = db.set_stage(id, Stage::Queued);
            let _ = db.save();
        }
        let mut queue = self.queue.lock().unwrap();
        if let Some(tail) = tail {
            queue.push(id, tail, now_ms());
        }
        queue.close(id, now_ms());
    }

    fn retry(&self, id: u64) {
        let (done, text) = {
            let db = self.db.lock().unwrap();
            match db.message(id) {
                Some(message) => (message.segments_done, message.text.clone()),
                None => return,
            }
        };
        // With audio on disk the take is decoded again from the start. Without
        // it, or without a cutter, the old behaviour stays: an empty job that
        // is closed by `finish`/the silence timeout.
        if !self.replay_audio(id, done, &text, now_ms()) {
            self.queue.lock().unwrap().open(id, done, &text, now_ms());
        }
    }

    fn cancel(&self, id: u64) {
        self.cutters.lock().unwrap().remove(&id);
        let mut sink = DbSink { db: &self.db };
        self.queue.lock().unwrap().cancel(id, &mut sink);
        // A cancelled message can never be retried, so its audio is dead
        // weight. Best effort: a leftover file is not a reason to fail.
        discard_audio(&self.db, id);
    }
}

/// PCM16 little endian in, f32 out: the same conversion the local API uses.
fn pcm16(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0)
        .collect()
}

/// Forget a message's persisted audio. Missing is the normal case.
fn discard_audio(db: &Mutex<Db>, id: u64) {
    let path = {
        let db = db.lock().unwrap();
        Db::audio_path(db.root(), id)
    };
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => crate::log::log(&format!(
            "site: cannot remove the audio of message {id}: {error}"
        )),
    }
}

/// The queue writes results here, and only here.
struct DbSink<'a> {
    db: &'a Mutex<Db>,
}

impl DbSink<'_> {
    /// Walk to `target` through the stages the machine allows.
    ///
    /// A leftover message may be sitting in `uploading` when its text is already
    /// complete: jumping to `done` would be refused, stepping through the queue
    /// is exactly what a real take does.
    fn advance(&self, id: u64, target: Stage) {
        let mut db = self.db.lock().unwrap();
        // Step only where the machine allows: asking `set_stage` for an illegal
        // jump is refused and logged, and a log full of refusals hides the real
        // one. A draft walks uploading → queued → transcribing, a leftover in
        // `uploading` whose text is already there walks the same way.
        for _ in 0..8 {
            let Some(stage) = db.message(id).map(|message| message.stage) else {
                break;
            };
            if stage == target {
                break;
            }
            // The destination first: a queued or transcribing take may now be closed
            // directly, and walking through the intermediate stages would stop at
            // the first legal step and never arrive.
            if db.set_stage(id, target) {
                break;
            }
            let Some(step) = [
                Stage::Uploading,
                Stage::Queued,
                Stage::Transcribing,
                target,
            ]
            .into_iter()
            .find(|step| stage.can_move_to(*step))
            else {
                break;
            };
            db.set_stage(id, step);
        }
        let _ = db.save();
    }
}

impl Sink for DbSink<'_> {
    fn progress(&mut self, id: u64, text: &str, segments_done: u32) {
        {
            let mut db = self.db.lock().unwrap();
            db.set_progress(id, text, segments_done);
            let _ = db.save();
        }
        // Text can arrive before the phone's `finish` set `queued` (the silence
        // timeout starts a decode on its own). A raw jump Uploading →
        // Transcribing is refused and the message looks stuck at "uploading"
        // while its text is already there; walk the allowed steps instead.
        self.advance(id, Stage::Transcribing);
    }

    fn complete(&mut self, id: u64, text: &str) {
        let done = {
            let db = self.db.lock().unwrap();
            db.message(id).map(|message| message.segments_done).unwrap_or(0)
        };
        {
            let mut db = self.db.lock().unwrap();
            db.set_progress(id, text, done);
            let _ = db.save();
        }
        self.advance(id, Stage::Done);
        // The text is final: nothing can be retried any more.
        discard_audio(self.db, id);
    }

    fn settle(&mut self, id: u64, stage: Stage) {
        self.advance(id, stage);
        if stage == Stage::Silent {
            // Silence is final too. `Failed` and `NoModel` keep the file: those
            // are exactly the stages a retry may come from.
            discard_audio(self.db, id);
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::queue::{Decoder, Outcome};

    /// Every push is one whole segment: the tests are about the plumbing, not
    /// about where speech pauses.
    struct OneSegment {
        count: u32,
    }
    impl Cutter for OneSegment {
        fn push(&mut self, samples: &[f32]) -> Vec<Segment> {
            if samples.is_empty() {
                return Vec::new();
            }
            self.count += 1;
            vec![Segment {
                index: self.count - 1,
                samples: samples.to_vec(),
            }]
        }
        fn flush(&mut self) -> Option<Segment> {
            None
        }
    }
    impl OneSegment {
        fn new() -> Box<dyn Cutter + Send> {
            Box::new(OneSegment { count: 0 })
        }
    }

    struct Says;
    impl Decoder for Says {
        fn decode(&mut self, _samples: &[f32]) -> Result<Outcome, String> {
            Ok(Outcome::Text(" раз".into()))
        }
    }

    /// A database in its own directory with one message ready to receive audio.
    fn fresh(name: &str) -> (Arc<Mutex<Db>>, u64) {
        let root = std::env::temp_dir().join(format!("vb-hub-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut db = Db::open(&root).unwrap();
        let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
        db.save().unwrap();
        let db = Arc::new(Mutex::new(db));
        let id = {
            let mut db = db.lock().unwrap();
            let message = db.add_message(monolog.id, "2026-11-12T15:00:00+05:00", "2026-11-12T15:00:01+05:00");
            db.save().unwrap();
            message.id
        };
        (db, id)
    }

    fn hub(name: &str) -> (Hub, u64) {
        let (db, id) = fresh(name);
        let hub = Hub::new(db);
        hub.listen(id, OneSegment::new(), 0);
        (hub, id)
    }

    /// The same hub, but built the way the runtime builds it: a retry has a
    /// cutter factory and can decode the file it kept.
    fn hub_with_cutters(name: &str) -> (Hub, u64) {
        let (db, id) = fresh(name);
        let factory: CutterFactory = Box::new(OneSegment::new);
        let hub = Hub::with_cutters(db, factory);
        hub.listen(id, OneSegment::new(), 0);
        (hub, id)
    }

    fn stage(hub: &Hub, id: u64) -> Stage {
        hub.db.lock().unwrap().message(id).unwrap().stage
    }

    fn audio_file(hub: &Hub, id: u64) -> std::path::PathBuf {
        Db::audio_path(hub.db.lock().unwrap().root(), id)
    }

    #[test]
    fn audio_becomes_progress_and_then_text() {
        let (hub, id) = hub("happy");
        let chunk = vec![0u8; 32_000]; // one second of silence, still audio
        hub.audio(id, &chunk);
        hub.audio(id, &chunk);
        hub.finish(id);
        let mut decoder = Says;

        let first = hub.pump(10, true, false, &mut decoder);
        assert!(matches!(first, Did::Decoded { segments_done: 1, .. }));
        assert_eq!(
            hub.db.lock().unwrap().message(id).unwrap().text,
            " раз",
            "the phone sees the text grow"
        );
        let second = hub.pump(20, true, false, &mut decoder);
        assert!(
            matches!(second, Did::Decoded { finished: true, .. }),
            "the second segment finishes the message"
        );
        assert_eq!(stage(&hub, id), Stage::Done);
    }

    #[test]
    fn without_a_model_the_message_says_so_and_keeps_the_audio() {
        let (hub, id) = hub("no-model");
        hub.audio(id, &vec![0u8; 32_000]);
        hub.finish(id);
        let mut decoder = Says;
        let did = hub.pump(10, false, false, &mut decoder);
        assert_eq!(did, Did::NoModel(vec![id]));
        assert_eq!(stage(&hub, id), Stage::NoModel);
        // The take is still there: a model can be chosen and the message retried.
        assert!(hub.db.lock().unwrap().message(id).is_some());
        assert!(
            audio_file(&hub, id).exists(),
            "no model is not a final answer: the audio must survive for a retry"
        );
    }

    #[test]
    fn a_busy_engine_leaves_the_message_queued() {
        let (hub, id) = hub("busy");
        hub.audio(id, &vec![0u8; 32_000]);
        hub.finish(id);
        let mut decoder = Says;
        assert_eq!(
            hub.pump(10, true, true, &mut decoder),
            Did::WaitingForEngine
        );
        assert_eq!(stage(&hub, id), Stage::Queued);
        assert!(matches!(
            hub.pump(20, true, false, &mut decoder),
            Did::Decoded { .. }
        ));
    }

    #[test]
    fn a_busy_engine_pauses_a_live_take_and_then_continues_where_it_stopped() {
        let (hub, id) = hub("live-busy");
        let mut decoder = Says;
        // The phone is still recording: one phrase was already decoded.
        hub.audio(id, &vec![0u8; 32_000]);
        assert!(matches!(
            hub.pump(10, true, false, &mut decoder),
            Did::Decoded {
                segments_done: 1,
                finished: false,
                ..
            }
        ));
        let before = hub.db.lock().unwrap().message(id).unwrap().text.clone();
        assert!(!before.is_empty());
        // The engine is taken by the person at the computer, and the phone keeps
        // talking: the new phrase waits instead of disappearing.
        hub.audio(id, &vec![0u8; 32_000]);
        assert_eq!(hub.pump(20, true, true, &mut decoder), Did::WaitingForEngine);
        {
            let db = hub.db.lock().unwrap();
            let message = db.message(id).unwrap();
            assert_eq!(message.segments_done, 1, "nothing decoded while busy");
            assert_eq!(message.text, before, "the text stands still while busy");
        }
        // The engine is free again: the take continues from the paused place.
        assert!(matches!(
            hub.pump(30, true, false, &mut decoder),
            Did::Decoded {
                segments_done: 2,
                finished: false,
                ..
            }
        ));
        let after = hub.db.lock().unwrap().message(id).unwrap().text.clone();
        assert!(
            after.starts_with(&before) && after.len() > before.len(),
            "the text continues: {before:?} then {after:?}"
        );
        hub.finish(id);
        assert!(matches!(
            hub.pump(40, true, false, &mut decoder),
            Did::Decoded { finished: true, .. }
        ));
        assert_eq!(stage(&hub, id), Stage::Done);
    }

    #[test]
    fn text_before_finish_does_not_leave_the_message_uploading() {
        let (hub, id) = hub("progress-first");
        // The first chunk makes the message `uploading`; the silence timeout
        // then starts a decode before the page sends `finish`.
        hub.audio(id, &vec![0u8; 32_000]);
        let mut sink = DbSink { db: &hub.db };
        sink.progress(id, " текст", 1);
        assert_eq!(
            hub.db.lock().unwrap().message(id).unwrap().stage,
            Stage::Transcribing,
            "a progress line must move uploading → queued → transcribing"
        );
    }

    #[test]
    fn a_first_chunk_during_a_decode_does_not_deadlock() {
        use std::sync::mpsc;

        struct Blocking {
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }
        impl Decoder for Blocking {
            fn decode(&mut self, _samples: &[f32]) -> Result<Outcome, String> {
                self.entered.send(()).unwrap();
                self.release.recv().unwrap();
                Ok(Outcome::Text(" раз".into()))
            }
        }

        let (hub, id) = hub("lock-order");
        hub.audio(id, &vec![0u8; 32_000]);
        hub.finish(id);
        let hub = std::sync::Arc::new(hub);

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        // The pump keeps the queue locked while it decodes.
        let pump_hub = hub.clone();
        let pump = std::thread::spawn(move || {
            let mut decoder = Blocking {
                entered: entered_tx,
                release: release_rx,
            };
            pump_hub.pump(5, true, false, &mut decoder)
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the decoder started");

        // A new take starts during that decode: this used to take the database
        // lock and then wait for the queue held by the pump.
        let (done_tx, done_rx) = mpsc::channel();
        let other = hub.clone();
        let listener = std::thread::spawn(move || {
            other.listen(999, OneSegment::new(), 0);
            let _ = done_tx.send(());
        });

        release_tx.send(()).unwrap();
        assert!(
            done_rx.recv_timeout(std::time::Duration::from_secs(3)).is_ok(),
            "listen must not wait for a queue held by the pump"
        );
        let _ = pump.join();
        let _ = listener.join();
    }

    #[test]
    fn the_hub_reports_background_work_until_the_take_is_done() {
        let (hub, id) = hub("has-work");
        assert!(hub.has_work(), "a take that is being received is work");
        hub.audio(id, &vec![0u8; 32_000]);
        hub.finish(id);
        let mut decoder = Says;
        for _ in 0..4 {
            hub.pump(10, true, false, &mut decoder);
        }
        assert!(!hub.has_work(), "the queue is empty again");
    }

    #[test]
    fn deleting_a_message_stops_the_queue() {
        let (hub, id) = hub("delete");
        hub.audio(id, &vec![0u8; 32_000]);
        hub.finish(id);
        hub.cancel(id);
        assert_eq!(stage(&hub, id), Stage::Cancelled);
        let mut decoder = Says;
        assert_eq!(hub.pump(10, true, false, &mut decoder), Did::Idle);
    }

    #[test]
    fn a_take_keeps_its_audio_for_a_retry() {
        let (hub, id) = hub_with_cutters("retry");
        let chunk = vec![0u8; 32_000];
        hub.audio(id, &chunk);
        hub.audio(id, &chunk);
        hub.audio(id, &chunk);
        hub.finish(id);
        let path = audio_file(&hub, id);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            96_000,
            "every byte the phone sent is in the file, not only the last chunk"
        );

        // Three broken decodes close the message as `failed`, which is a
        // stage a retry may come from.
        struct Broken;
        impl Decoder for Broken {
            fn decode(&mut self, _samples: &[f32]) -> Result<Outcome, String> {
                Err("boom".into())
            }
        }
        let mut broken = Broken;
        for step in 0..3 {
            hub.pump(10 + step, true, false, &mut broken);
        }
        assert_eq!(stage(&hub, id), Stage::Failed);

        hub.retry(id);
        let mut decoder = Says;
        let mut finished = false;
        for step in 0..4 {
            let did = hub.pump(100 + step, true, false, &mut decoder);
            finished |= matches!(did, Did::Decoded { finished: true, .. });
        }
        assert!(finished, "the replayed segments must reach `done`");
        assert_eq!(stage(&hub, id), Stage::Done);
        assert_eq!(
            hub.db.lock().unwrap().message(id).unwrap().text,
            " раз",
            "the text came from the kept audio"
        );
        assert!(!path.exists(), "a finished take has no retry left");
    }

    #[test]
    fn a_retry_without_audio_does_not_panic() {
        // No file on disk and no factory: the old empty-job behaviour stays.
        let (hub, id) = hub("retry-no-audio");
        assert!(!audio_file(&hub, id).exists());
        hub.retry(id);
        let mut decoder = Says;
        assert_eq!(hub.pump(10, true, false, &mut decoder), Did::Idle);

        // A factory without a file must not be worse.
        let (hub, id) = hub_with_cutters("retry-no-audio-cutter");
        assert!(!audio_file(&hub, id).exists());
        hub.retry(id);
        assert_eq!(hub.pump(10, true, false, &mut decoder), Did::Idle);
    }

    #[test]
    fn cancelling_a_take_removes_its_audio() {
        let (hub, id) = hub_with_cutters("cancel-audio");
        hub.audio(id, &vec![0u8; 32_000]);
        hub.finish(id);
        let path = audio_file(&hub, id);
        assert!(path.exists(), "the take was being kept");
        hub.cancel(id);
        assert!(!path.exists(), "a cancelled take can never be retried");
    }

    #[test]
    fn a_message_left_by_the_last_run_is_finished_not_forgotten() {
        let root = std::env::temp_dir().join(format!("vb-hub-resume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut db = Db::open(&root).unwrap();
        let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
        let with_text = db.add_message(monolog.id, "2026-11-12T15:00:00+05:00", "2026-11-12T15:00:01+05:00");
        db.add_seconds(with_text.id, 5.0);
        db.set_progress(with_text.id, "успел сказать", 2);
        let empty = db.add_message(monolog.id, "2026-11-12T15:05:00+05:00", "2026-11-12T15:05:01+05:00");
        db.save().unwrap();
        let hub = Hub::new(Arc::new(Mutex::new(Db::open(&root).unwrap())));

        hub.resume(0);
        let mut decoder = Says;
        // Both leftovers are closed; the queue finalizes them.
        for _ in 0..2 {
            hub.pump(10, true, false, &mut decoder);
        }
        let db = hub.db.lock().unwrap();
        assert_eq!(db.message(with_text.id).unwrap().stage, Stage::Done);
        assert_eq!(db.message(with_text.id).unwrap().text, "успел сказать");
        assert_eq!(db.message(empty.id).unwrap().stage, Stage::Silent);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_leftover_reaches_its_answer_without_illegal_steps() {
        let root = std::env::temp_dir().join(format!("vb-hub-advance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut db = Db::open(&root).unwrap();
        let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
        let done = db.add_message(
            monolog.id,
            "2026-11-12T15:00:00+05:00",
            "2026-11-12T15:00:01+05:00",
        );
        let silent = db.add_message(
            monolog.id,
            "2026-11-12T15:05:00+05:00",
            "2026-11-12T15:05:01+05:00",
        );
        // The last run died while the phone was still sending.
        assert!(db.set_stage(done.id, Stage::Uploading));
        assert!(db.set_stage(silent.id, Stage::Uploading));
        db.save().unwrap();
        let db = Mutex::new(Db::open(&root).unwrap());
        let mut sink = DbSink { db: &db };
        sink.advance(done.id, Stage::Done);
        sink.settle(silent.id, Stage::Silent);
        let db = db.lock().unwrap();
        assert_eq!(db.message(done.id).unwrap().stage, Stage::Done);
        assert_eq!(db.message(silent.id).unwrap().stage, Stage::Silent);
        drop(db);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_queue_holds_one_cutter_per_message() {
        let (hub, id) = hub("cutters");
        hub.audio(id, &vec![0u8; 4_000]);
        hub.finish(id);
        // Audio after the stop must not resurrect the cut take.
        hub.audio(id, &vec![0u8; 4_000]);
        let mut decoder = Says;
        assert!(matches!(
            hub.pump(10, true, false, &mut decoder),
            Did::Decoded { .. }
        ));
        let db = hub.db.lock().unwrap();
        assert!(db.message(id).unwrap().duration <= 0.5, "the extra audio was dropped");
    }
}
