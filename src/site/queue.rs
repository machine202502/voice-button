//! The transcription queue for phone messages.
//!
//! One queue, one decoder, and a hard rule: the phone never takes the engine
//! away from the person at the computer. The queue therefore asks three
//! questions before every segment — is there a model, is the engine busy, is
//! there work — and does exactly one thing per call, so every path is testable
//! without a real model or a real timer.
//!
//! Stages owned here: `queued → transcribing → done | silent | failed |
//! no_model`, plus `cancelled` when the user deletes a message.

use std::collections::{BTreeMap, VecDeque};

use super::monolog::Stage;

/// A piece of audio ready to be decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub index: u32,
    pub samples: Vec<f32>,
}

/// What the decoder made of a segment.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Text(String),
    /// Speech was expected but nothing came out of the engine.
    Empty,
}

pub trait Decoder {
    fn decode(&mut self, samples: &[f32]) -> Result<Outcome, String>;
}

/// Where results go. Storage and the window are behind this, so the queue
/// itself has no idea about files or events.
pub trait Sink {
    fn progress(&mut self, id: u64, text: &str, segments_done: u32);
    fn complete(&mut self, id: u64, text: &str);
    fn settle(&mut self, id: u64, stage: Stage);
}

/// How long a message may receive nothing before it is closed.
pub const SILENCE_MS: u64 = 45_000;
/// How many decodes in a row may fail before the message gives up.
pub const FAILURES: usize = 3;

#[derive(Debug)]
struct Job {
    /// Segments waiting, oldest first.
    segments: VecDeque<Segment>,
    /// Segments already transcribed.
    done: u32,
    /// The phone said it has nothing more to send.
    closed: bool,
    /// Text accumulated so far.
    text: String,
    failures: usize,
    /// When audio or a stop last arrived.
    touched: u64,
}

impl Job {
    fn has_work(&self) -> bool {
        !self.segments.is_empty()
    }

    fn finished(&self) -> bool {
        self.closed && !self.has_work()
    }
}

#[derive(Debug, Default)]
pub struct Queue {
    /// Jobs in arrival order; `order` is the priority list.
    jobs: BTreeMap<u64, Job>,
    order: VecDeque<u64>,
}

/// What one call to `tick` did, for logging and for tests.
#[derive(Debug, PartialEq)]
pub enum Did {
    /// Nothing to do.
    Idle,
    /// The engine is somebody else's right now.
    WaitingForEngine,
    /// No model: every waiting message was closed as `no_model`.
    NoModel(Vec<u64>),
    /// One segment was decoded; `finished` says the message is complete.
    Decoded {
        id: u64,
        segments_done: u32,
        finished: bool,
    },
    /// A message was closed with no text.
    Silent(u64),
    /// A decode failed; `given_up` says the message is now `failed`.
    Failed { id: u64, given_up: bool },
    /// A message was left without audio for too long and is closed.
    TimedOut(u64),
    /// The message was cancelled.
    Cancelled(u64),
}

impl Queue {
    /// Start (or resume) a message. `done` is how many segments were already
    /// transcribed before the app restarted.
    pub fn open(&mut self, id: u64, done: u32, text: &str, now: u64) {
        let job = Job {
            segments: VecDeque::new(),
            done,
            closed: false,
            text: text.to_string(),
            failures: 0,
            touched: now,
        };
        self.jobs.insert(id, job);
        if !self.order.contains(&id) {
            self.order.push_back(id);
        }
    }

    /// Audio arrived for a message; it also counts as a sign of life.
    pub fn push(&mut self, id: u64, segment: Segment, now: u64) {
        if let Some(job) = self.jobs.get_mut(&id) {
            job.touched = now;
            job.segments.push_back(segment);
        }
    }

    /// The phone stopped sending: the tail may now be decoded.
    pub fn close(&mut self, id: u64, now: u64) {
        if let Some(job) = self.jobs.get_mut(&id) {
            job.touched = now;
            job.closed = true;
        }
    }

    pub fn cancel(&mut self, id: u64, sink: &mut dyn Sink) -> Did {
        self.order.retain(|queued| *queued != id);
        self.jobs.remove(&id);
        sink.settle(id, Stage::Cancelled);
        Did::Cancelled(id)
    }

    /// Messages that are still waiting for work.
    pub fn waiting(&self) -> Vec<u64> {
        self.order.iter().copied().collect()
    }

    /// Does the queue hold work? While it does, the phone is a background
    /// task and the tray icon says so.
    pub fn busy(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// Has the phone already said `finish` for this take? A job that exists
    /// and is closed must refuse late audio even though its DB stage is still
    /// `queued` or `transcribing`.
    pub fn take_closed(&self, id: u64) -> bool {
        self.jobs.get(&id).map(|job| job.closed).unwrap_or(false)
    }

    /// Is there audio the engine could decode right now? This, and not "some
    /// take exists", is what makes the tray icon violet.
    pub fn has_segments(&self) -> bool {
        self.jobs.values().any(|job| !job.segments.is_empty())
    }

    pub fn stage_of(&self, id: u64) -> Option<Stage> {
        self.jobs.get(&id).map(|job| {
            if job.finished() {
                Stage::Done
            } else if job.segments.is_empty() {
                Stage::Queued
            } else {
                Stage::Transcribing
            }
        })
    }

    /// Decide what to do next and do it. `now` is milliseconds from any fixed
    /// point: the caller owns the clock, so tests do not sleep.
    pub fn tick(
        &mut self,
        _now: u64,
        model_ready: bool,
        engine_busy: bool,
        decoder: &mut dyn Decoder,
        sink: &mut dyn Sink,
    ) -> Did {
        // A missing model is not something to wait for: nothing will change
        // until the user chooses one, and the phone must not show "transcribing"
        // in the meantime. The hub keeps the raw audio in `site/audio` for a
        // retry, so nothing here has to hold it.
        //
        // A take that is still sending audio is not finished, though: closing
        // it here would make the API refuse the next chunk and throw the rest
        // of the recording away. Only a take the phone has stopped — or one
        // that has been silent for too long — is settled as `no_model`.
        if !model_ready {
            let waiting: Vec<u64> = self
                .order
                .iter()
                .copied()
                .filter(|id| self.jobs.get(id).map(|job| job.closed).unwrap_or(true))
                .collect();
            for id in &waiting {
                self.jobs.remove(id);
                self.order.retain(|queued| queued != id);
                sink.settle(*id, Stage::NoModel);
            }
            return if waiting.is_empty() {
                Did::Idle
            } else {
                Did::NoModel(waiting)
            };
        }

        // A take stays open until the phone says `finish`; a quiet phone is
        // not a finished take. The person may simply be thinking, and closing
        // it meant the rest of the recording was refused as "not receiving
        // audio". A job without new segments is skipped, so other takes are
        // not blocked while it waits.

        if engine_busy {
            return Did::WaitingForEngine;
        }

        // The oldest take that has something for the engine. A take that is
        // merely open (the person went quiet, or the phone is away) is skipped
        // instead of holding every later take behind it.
        let Some(id) = self.order.iter().copied().find(|id| match self.jobs.get(id) {
            Some(job) => job.finished() || job.has_work(),
            None => true,
        }) else {
            return Did::Idle;
        };
        let Some(job) = self.jobs.get(&id) else {
            self.order.retain(|queued| *queued != id);
            return Did::Idle;
        };
        // Copy what is needed while the job is only borrowed for reading.
        let (finished, text, done) = (job.finished(), job.text.clone(), job.done);

        if finished {
            self.jobs.remove(&id);
            self.order.retain(|queued| *queued != id);
            // Nothing was transcribed at all: the audio held no speech.
            if text.trim().is_empty() {
                sink.settle(id, Stage::Silent);
                return Did::Silent(id);
            }
            sink.complete(id, &text);
            return Did::Decoded {
                id,
                segments_done: done,
                finished: true,
            };
        }

        if self.jobs.get(&id).map(Job::has_work).unwrap_or(false) {
            let segment = self
                .jobs
                .get_mut(&id)
                .and_then(|job| job.segments.pop_front())
                .expect("checked above");
            let result = decoder.decode(&segment.samples);
            match result {
                Ok(Outcome::Text(text)) => {
                    let job = self.jobs.get_mut(&id).expect("still open");
                    job.text.push_str(&text);
                    job.done += 1;
                    job.failures = 0;
                    let progress = job.text.clone();
                    let done = job.done;
                    sink.progress(id, &progress, done);
                    let finished = job.closed && job.segments.is_empty();
                    if finished {
                        let text = job.text.clone();
                        self.jobs.remove(&id);
                        self.order.retain(|queued| *queued != id);
                        sink.complete(id, &text);
                    }
                    return Did::Decoded {
                        id,
                        segments_done: done,
                        finished,
                    };
                }
                Ok(Outcome::Empty) => {
                    let job = self.jobs.get_mut(&id).expect("still open");
                    // A segment with no words still counts as done, otherwise
                    // the message would try it forever.
                    job.done += 1;
                    job.failures = 0;
                    let progress = job.text.clone();
                    let done = job.done;
                    sink.progress(id, &progress, done);
                    let finished = job.closed && job.segments.is_empty();
                    if finished {
                        self.jobs.remove(&id);
                        self.order.retain(|queued| *queued != id);
                        if progress.trim().is_empty() {
                            sink.settle(id, Stage::Silent);
                            return Did::Silent(id);
                        }
                        sink.complete(id, &progress);
                    }
                    return Did::Decoded {
                        id,
                        segments_done: done,
                        finished,
                    };
                }
                Err(error) => {
                    crate::log::log(&format!("site: message {id}: {error}"));
                    let job = self.jobs.get_mut(&id).expect("still open");
                    job.failures += 1;
                    let given_up = job.failures >= FAILURES;
                    if given_up {
                        self.jobs.remove(&id);
                        self.order.retain(|queued| *queued != id);
                        sink.settle(id, Stage::Failed);
                    }
                    return Did::Failed { id, given_up };
                }
            }
        }

        Did::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Sink2 {
        progress: Vec<(u64, String, u32)>,
        complete: Vec<(u64, String)>,
        settled: Vec<(u64, Stage)>,
    }

    impl Sink for Sink2 {
        fn progress(&mut self, id: u64, text: &str, segments_done: u32) {
            self.progress.push((id, text.to_string(), segments_done));
        }
        fn complete(&mut self, id: u64, text: &str) {
            self.complete.push((id, text.to_string()));
        }
        fn settle(&mut self, id: u64, stage: Stage) {
            self.settled.push((id, stage));
        }
    }

    /// Says "раз" for every segment unless told otherwise.
    struct Scripted {
        replies: VecDeque<Result<Outcome, String>>,
        asked: usize,
    }

    impl Scripted {
        fn good(count: usize) -> Self {
            Self {
                replies: (0..count).map(|_| Ok(Outcome::Text(" раз".into()))).collect(),
                asked: 0,
            }
        }
        fn from(replies: Vec<Result<Outcome, String>>) -> Self {
            Self {
                replies: replies.into(),
                asked: 0,
            }
        }
    }

    impl Decoder for Scripted {
        fn decode(&mut self, _samples: &[f32]) -> Result<Outcome, String> {
            self.asked += 1;
            self.replies
                .pop_front()
                .unwrap_or(Ok(Outcome::Text(" раз".into())))
        }
    }

    fn segment(index: u32) -> Segment {
        Segment {
            index,
            samples: vec![0.1; 16_000],
        }
    }

    #[test]
    fn one_segment_becomes_progress_and_then_the_text() {
        let mut queue = Queue::default();
        let mut sink = Sink2::default();
        let mut decoder = Scripted::good(2);
        queue.open(1, 0, "", 0);
        queue.push(1, segment(0), 10);
        queue.push(1, segment(1), 20);

        assert_eq!(
            queue.tick(30, true, false, &mut decoder, &mut sink),
            Did::Decoded {
                id: 1,
                segments_done: 1,
                finished: false
            }
        );
        assert_eq!(sink.progress.len(), 1, "the first segment is visible at once");

        queue.close(1, 40);
        let last = queue.tick(50, true, false, &mut decoder, &mut sink);
        assert_eq!(
            last,
            Did::Decoded {
                id: 1,
                segments_done: 2,
                finished: true
            }
        );
        assert_eq!(sink.complete, vec![(1, " раз раз".to_string())]);
        assert!(queue.waiting().is_empty(), "the queue is done with it");
    }

    #[test]
    fn speech_that_yields_nothing_is_silence() {
        let mut queue = Queue::default();
        let mut sink = Sink2::default();
        let mut decoder = Scripted::from(vec![Ok(Outcome::Empty)]);
        queue.open(1, 0, "", 0);
        queue.push(1, segment(0), 10);
        queue.close(1, 20);
        let did = queue.tick(30, true, false, &mut decoder, &mut sink);
        assert_eq!(did, Did::Silent(1));
        assert_eq!(sink.settled, vec![(1, Stage::Silent)]);
    }

    #[test]
    fn without_a_model_nothing_is_transcribed() {
        let mut queue = Queue::default();
        let mut sink = Sink2::default();
        let mut decoder = Scripted::good(1);
        queue.open(1, 0, "", 0);
        queue.open(2, 0, "", 0);
        queue.push(1, segment(0), 10);
        // A take that is still receiving audio is not finished: settling it now
        // would make the API refuse the rest of the recording.
        assert_eq!(
            queue.tick(30, false, false, &mut decoder, &mut sink),
            Did::Idle
        );
        assert_eq!(decoder.asked, 0, "the engine was never asked");
        assert!(sink.settled.is_empty());

        // Once the phone stops, every take says so at once.
        queue.close(1, 40);
        queue.close(2, 40);
        assert_eq!(
            queue.tick(50, false, false, &mut decoder, &mut sink),
            Did::NoModel(vec![1, 2])
        );
        assert_eq!(decoder.asked, 0, "the engine was never asked");
        assert_eq!(
            sink.settled,
            vec![(1, Stage::NoModel), (2, Stage::NoModel)]
        );
    }

    #[test]
    fn nothing_runs_while_the_engine_is_busy() {
        let mut queue = Queue::default();
        let mut sink = Sink2::default();
        let mut decoder = Scripted::good(1);
        queue.open(1, 0, "", 0);
        queue.push(1, segment(0), 10);
        assert_eq!(
            queue.tick(20, true, true, &mut decoder, &mut sink),
            Did::WaitingForEngine
        );
        assert_eq!(decoder.asked, 0);
        assert!(sink.progress.is_empty());
        // When the engine is free, the very same segment goes through.
        assert!(matches!(
            queue.tick(30, true, false, &mut decoder, &mut sink),
            Did::Decoded { .. }
        ));
    }

    #[test]
    fn a_quiet_phone_is_not_a_finished_take() {
        let mut queue = Queue::default();
        let mut sink = Sink2::default();
        let mut decoder = Scripted::good(1);
        queue.open(1, 0, "", 0);
        queue.push(1, segment(0), 10);
        // Ten minutes without a byte from the phone: the take stays open, the
        // audio already here is decoded, and nothing is declared finished.
        let did = queue.tick(600_000, true, false, &mut decoder, &mut sink);
        assert!(
            matches!(did, Did::Decoded { finished: false, .. }),
            "the segment is still decoded, the take is not closed: {did:?}"
        );
        assert!(sink.complete.is_empty(), "finish alone ends a take");
        assert_eq!(queue.stage_of(1), Some(Stage::Queued), "still open, no segments");
    }

    #[test]
    fn a_quiet_take_does_not_block_the_next_one() {
        let mut queue = Queue::default();
        let mut sink = Sink2::default();
        let mut decoder = Scripted::good(1);
        // Take 1 was opened and then the phone went quiet; take 2 has audio.
        queue.open(1, 0, "", 0);
        queue.open(2, 0, "", 0);
        queue.push(2, segment(0), 10);
        let did = queue.tick(20, true, false, &mut decoder, &mut sink);
        assert!(
            matches!(did, Did::Decoded { id: 2, .. }),
            "the quiet take must not hold the queue: {did:?}"
        );
    }

    #[test]
    fn a_broken_engine_gives_up_after_three_tries() {
        let mut queue = Queue::default();
        let mut sink = Sink2::default();
        let mut decoder = Scripted::from(vec![
            Err("boom".into()),
            Err("boom".into()),
            Err("boom".into()),
        ]);
        queue.open(1, 0, "", 0);
        queue.push(1, segment(0), 10);
        queue.push(1, segment(1), 20);
        queue.push(1, segment(2), 30);
        for _ in 0..2 {
            assert!(matches!(
                queue.tick(40, true, false, &mut decoder, &mut sink),
                Did::Failed { given_up: false, .. }
            ));
        }
        assert_eq!(
            queue.tick(50, true, false, &mut decoder, &mut sink),
            Did::Failed {
                id: 1,
                given_up: true
            }
        );
        assert_eq!(sink.settled, vec![(1, Stage::Failed)]);
    }

    #[test]
    fn a_resumed_message_continues_after_the_last_segment() {
        let mut queue = Queue::default();
        let mut sink = Sink2::default();
        let mut decoder = Scripted::good(1);
        // Two segments were transcribed before the restart.
        queue.open(1, 2, " старый текст", 0);
        queue.push(1, segment(2), 10);
        queue.close(1, 20);
        let did = queue.tick(30, true, false, &mut decoder, &mut sink);
        assert_eq!(
            did,
            Did::Decoded {
                id: 1,
                segments_done: 3,
                finished: true
            }
        );
        assert_eq!(
            sink.complete,
            vec![(1, " старый текст раз".to_string())],
            "the earlier text is kept, not restarted"
        );
    }

    #[test]
    fn cancelling_removes_the_message_and_says_so() {
        let mut queue = Queue::default();
        let mut sink = Sink2::default();
        queue.open(1, 0, "", 0);
        queue.push(1, segment(0), 10);
        assert_eq!(queue.cancel(1, &mut sink), Did::Cancelled(1));
        assert!(queue.waiting().is_empty());
        assert_eq!(sink.settled, vec![(1, Stage::Cancelled)]);
    }

    #[test]
    fn messages_are_served_in_the_order_they_were_recorded() {
        let mut queue = Queue::default();
        let mut sink = Sink2::default();
        let mut decoder = Scripted::good(2);
        queue.open(7, 0, "", 0);
        queue.open(3, 0, "", 0);
        queue.push(7, segment(0), 10);
        queue.push(3, segment(0), 10);
        assert!(matches!(
            queue.tick(20, true, false, &mut decoder, &mut sink),
            Did::Decoded { id: 7, .. }
        ));
    }
}
