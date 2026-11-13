//! One recognizer, several callers, one order.
//!
//! Everything that transcribes ends up in [`Engine::decode`]: the microphone,
//! an imported file, the local API, the phone and now Telegram. The scheduler
//! here decides whose segment runs next. A segment already being decoded is
//! never killed — preemption happens at the next segment boundary, because the
//! ASR worker has no safe way to stop in the middle of one.
//!
//! Equal priorities are served first-come-first-served. Every fourth grant is
//! given to the oldest waiter regardless of its priority, so a constant stream
//! of dictation cannot starve the phone or Telegram for ever.

use crate::asr::{AsrClient, Reply};
use anyhow::Result;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// A file the person imported: below live dictation, above the phone.
    User = 4,
    /// The local API: a caller is waiting for an answer.
    Api = 3,
    File = 2,
    Phone = 1,
    Telegram = 0,
}

impl Priority {
    fn rank(self) -> u64 {
        self as u64
    }
}

#[derive(Clone)]
struct Waiter {
    priority: Priority,
    seq: u64,
    since: Instant,
}

#[derive(Default)]
struct State {
    busy: bool,
    next_seq: u64,
    grants: u64,
    waiters: Vec<Waiter>,
}

pub struct Engine {
    client: Mutex<AsrClient>,
    state: Mutex<State>,
    changed: Condvar,
}

/// Every fourth grant ignores priority and serves the oldest waiter.
const RESERVED_EVERY: u64 = 4;

impl Engine {
    pub fn new(exe: std::path::PathBuf) -> Self {
        Self {
            client: Mutex::new(AsrClient::new(exe)),
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        }
    }

    /// Decode one segment. The caller keeps its priority only for that segment;
    /// the next segment asks again, so a higher-priority source can take over
    /// between segments.
    pub fn decode(&self, priority: Priority, model: &str, samples: &[f32], rate: u32) -> Result<Reply> {
        let mut state = self.state.lock().unwrap();
        let seq = state.next_seq;
        state.next_seq += 1;
        state.waiters.push(Waiter {
            priority,
            seq,
            since: Instant::now(),
        });
        loop {
            let chosen = pick(&state, Instant::now());
            if !state.busy && chosen == Some(seq) {
                state.waiters.retain(|waiter| waiter.seq != seq);
                state.busy = true;
                state.grants += 1;
                break;
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, Duration::from_millis(250))
                .unwrap();
            state = next;
        }
        drop(state);
        let result = self.client.lock().unwrap().decode(model, samples, rate);
        {
            let mut state = self.state.lock().unwrap();
            state.busy = false;
            self.changed.notify_all();
        }
        result
    }
}

/// Which waiter goes next. Pure, so the ordering rules can be tested without
/// threads or real time.
fn pick(state: &State, now: Instant) -> Option<u64> {
    if state.waiters.is_empty() {
        return None;
    }
    let reserved = state.grants % RESERVED_EVERY == RESERVED_EVERY - 1;
    if reserved {
        return state
            .waiters
            .iter()
            .min_by_key(|waiter| waiter.seq)
            .map(|waiter| waiter.seq);
    }
    state
        .waiters
        .iter()
        .max_by(|left, right| {
            let left_score = age_score(left, now);
            let right_score = age_score(right, now);
            left_score
                .cmp(&right_score)
                // Higher priority wins; on equal priority the earlier request
                // wins. `max_by` sees the smaller sequence as the greater one.
                .then_with(|| right.seq.cmp(&left.seq))
        })
        .map(|waiter| waiter.seq)
}

fn age_score(waiter: &Waiter, now: Instant) -> u64 {
    let waited = now.saturating_duration_since(waiter.since).as_millis() as u64;
    // After every two seconds a waiter moves one priority up, up to two levels.
    waiter.priority.rank() + (waited / 2_000).min(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiter(priority: Priority, seq: u64, since: Instant) -> Waiter {
        Waiter {
            priority,
            seq,
            since,
        }
    }

    fn with_waiters(waiters: Vec<Waiter>, grants: u64) -> State {
        State {
            busy: false,
            next_seq: 0,
            grants,
            waiters,
        }
    }

    #[test]
    fn a_higher_priority_waiter_goes_first() {
        let now = Instant::now();
        let state = with_waiters(
            vec![
                waiter(Priority::Telegram, 0, now),
                waiter(Priority::User, 1, now),
            ],
            0,
        );
        assert_eq!(pick(&state, now), Some(1));
    }

    #[test]
    fn equal_priorities_keep_their_arrival_order() {
        let now = Instant::now();
        let state = with_waiters(
            vec![
                waiter(Priority::Phone, 4, now),
                waiter(Priority::Phone, 2, now),
                waiter(Priority::Phone, 3, now),
            ],
            0,
        );
        assert_eq!(pick(&state, now), Some(2));
    }

    #[test]
    fn waiting_raises_the_priority_of_a_low_waiter() {
        let now = Instant::now();
        let state = with_waiters(
            vec![
                waiter(Priority::Telegram, 0, now),
                waiter(Priority::Phone, 1, now - Duration::from_secs(5)),
            ],
            0,
        );
        // Telegram has waited long enough to reach the phone's level, but the
        // phone is still earlier; on the next second the telegram request is
        // not allowed to overtake a higher priority that is also waiting.
        assert_eq!(pick(&state, now), Some(1));
        let now = now + Duration::from_secs(2);
        let state = with_waiters(
            vec![
                waiter(Priority::Telegram, 0, now - Duration::from_secs(8)),
                waiter(Priority::File, 1, now - Duration::from_secs(1)),
            ],
            0,
        );
        // Telegram: 0 + 2 = 2, File: 2 + 0 = 2; earlier wins.
        assert_eq!(pick(&state, now), Some(0));
    }

    #[test]
    fn every_fourth_grant_is_reserved_for_the_oldest_waiter() {
        let now = Instant::now();
        let waiters = vec![
            waiter(Priority::User, 10, now),
            waiter(Priority::Telegram, 3, now),
        ];
        let normal = with_waiters(waiters.clone(), 0);
        assert_eq!(pick(&normal, now), Some(10));
        let reserved = with_waiters(waiters, RESERVED_EVERY - 1);
        assert_eq!(pick(&reserved, now), Some(3), "the oldest waiter gets a turn");
    }

    #[test]
    fn a_busy_engine_is_not_preempted_mid_segment() {
        let now = Instant::now();
        let mut state = with_waiters(vec![waiter(Priority::User, 1, now)], 0);
        state.busy = true;
        // The scheduler can only choose after the current segment is released;
        // busy is checked by `decode`, and `pick` still reports the waiter.
        assert_eq!(pick(&state, now), Some(1));
    }
}
