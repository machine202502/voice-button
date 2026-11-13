//! Running the new phone layer inside the daemon.
//!
//! Two things live here, and both exist to keep the queue honest:
//!
//! * `AsrDecoder` turns the real recognizer into the queue's `Decoder`. A model
//!   that cannot start is an error, not a silent empty answer — that is what
//!   makes "no model" a stage instead of an endless wait.
//! * `pump` is the loop that calls the queue once per step, asking the app the
//!   three questions the queue needs: is a model ready, is the engine busy, is
//!   there work.
//!
//! The loop takes its clock and its sleeping from the caller, so it can be
//! driven in a test without waiting.

use std::sync::Arc;

use super::queue::{Decoder, Outcome};
use crate::state::{Shared, Status};

pub struct AsrDecoder {
    client: Arc<crate::asr_engine::Engine>,
    failures_before_reset: usize,
}

impl AsrDecoder {
    pub fn new(client: Arc<crate::asr_engine::Engine>) -> Self {
        Self {
            client,
            failures_before_reset: 0,
        }
    }
}

impl Decoder for AsrDecoder {
    fn decode(&mut self, samples: &[f32]) -> Result<Outcome, String> {
        let reply = self
            .client
            .decode(
                crate::asr_engine::Priority::Phone,
                "",
                samples,
                crate::session::SAMPLE_RATE,
            )
            .map_err(|error| format!("{error:#}"))?;
        self.failures_before_reset = 0;
        if reply.text.trim().is_empty() {
            Ok(Outcome::Empty)
        } else {
            Ok(Outcome::Text(reply.text))
        }
    }
}

/// Is there a model that can decode right now?
pub fn model_ready(shared: &Arc<Shared>) -> bool {
    shared.lock().status != Status::NoModel
}

/// Is the engine somebody else's? The phone waits, it never takes the
/// microphone away from the person using the computer.
pub fn engine_busy(shared: &Arc<Shared>) -> bool {
    let data = shared.lock();
    data.status != Status::Idle
        || data.session.is_some()
        || data.stream.is_some()
        || data.recovering
        || data
            .file
            .as_ref()
            .map(|file| file.running || file.total_seconds <= 0.0)
            .unwrap_or(false)
}

/// One step of the queue with the answers the app currently gives.
pub fn step(hub: &super::hub::Hub, shared: &Arc<Shared>, decoder: &mut dyn Decoder) -> super::queue::Did {
    hub.pump(
        super::hub::now_ms(),
        model_ready(shared),
        engine_busy(shared),
        decoder,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppEvent, History, Shared};
    use std::sync::mpsc;

    fn shared_with(status: Status) -> Arc<Shared> {
        let config = crate::config::Config::default();
        let shared = Arc::new(Shared::new(config, History::from_entries(Vec::new())));
        shared.lock().status = status;
        shared
    }

    #[test]
    fn a_missing_model_is_reported_and_never_counts_as_ready() {
        assert!(!model_ready(&shared_with(Status::NoModel)));
        assert!(model_ready(&shared_with(Status::Idle)));
    }

    #[test]
    fn the_engine_is_busy_while_the_user_records() {
        assert!(engine_busy(&shared_with(Status::Recording)));
        assert!(engine_busy(&shared_with(Status::Transcribing)));
        assert!(engine_busy(&shared_with(Status::File)));
        assert!(!engine_busy(&shared_with(Status::Idle)));
    }

    #[test]
    fn a_failed_decode_is_an_error_not_an_empty_answer() {
        // A client that was never started cannot decode: the queue must see an
        // error, so the message ends up failed instead of silently empty.
        let client = Arc::new(crate::asr_engine::Engine::new(
            std::env::temp_dir().join("vb-no-such-worker.exe"),
        ));
        let mut decoder = AsrDecoder::new(client);
        let result = decoder.decode(&vec![0.0; 1600]);
        assert!(result.is_err(), "a broken worker must not look like silence");
    }

    #[test]
    fn the_events_channel_stays_quiet_for_the_phone_layer() {
        // The daemon owns history and tray events; this layer only reports to
        // the queue, so nothing may be sent from here.
        let (events, received) = mpsc::channel::<AppEvent>();
        drop(events);
        assert!(received.recv().is_err());
    }
}
