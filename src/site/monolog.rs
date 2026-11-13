//! The vocabulary of the phone half of the app.
//!
//! A **monolog** is what used to be called a group: one named thread of
//! dictations. A **message** inside a monolog is one spoken take, and it moves
//! through a closed list of stages. Nothing outside this module decides what a
//! stage may become, so the interface can never show a state the engine has not
//! actually reached.
//!
//! ```text
//! draft → uploading → queued → transcribing → done
//!                                    ├→ silent     (no speech in the audio)
//!                                    ├→ no_model   (nothing to decode with)
//!                                    └→ failed     (the engine broke)
//!   any of the open stages ─→ cancelled  (the user threw it away)
//! ```

use serde::{Deserialize, Serialize};

/// Where a message is, as far as the computer is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// The phone started a take: the message exists, no audio yet.
    #[default]
    Draft,
    /// Audio is arriving.
    Uploading,
    /// The phone finished sending; the message waits for the engine.
    Queued,
    /// The engine is working on it right now.
    Transcribing,
    /// Text is there and it is the final answer.
    Done,
    /// The audio held no speech at all.
    Silent,
    /// No model is selected, so nothing can be decoded. The audio is kept.
    NoModel,
    /// The engine failed.
    Failed,
    /// The user deleted it.
    Cancelled,
}

impl Stage {
    /// Every stage a message may legally move to from here.
    pub fn next(self) -> &'static [Stage] {
        use Stage::*;
        match self {
            Draft => &[Uploading, Cancelled, Failed, NoModel],
            Uploading => &[Queued, Cancelled, Failed, NoModel],
            Queued => &[Transcribing, Cancelled, Failed, NoModel],
            Transcribing => &[Done, Silent, Failed, NoModel, Cancelled],
            // A finished message stays finished: only the phone's own copy may
            // change afterwards, never the recorded result here.
            Done => &[],
            Silent => &[],
            NoModel => &[Queued, Cancelled],
            Failed => &[Queued, Cancelled],
            Cancelled => &[],
        }
    }

    pub fn can_move_to(self, next: Stage) -> bool {
        self.next().contains(&next)
    }

    /// Stages that will not change on their own.
    pub fn is_final(self) -> bool {
        matches!(self, Stage::Done | Stage::Silent | Stage::Cancelled)
    }

    /// Stages where the computer still has work to do.
    pub fn is_open(self) -> bool {
        !self.is_final() && self != Stage::NoModel && self != Stage::Failed
    }

    /// The word the interface shows, in one place.
    pub fn label(self) -> &'static str {
        use Stage::*;
        match self {
            Draft => "draft",
            Uploading => "uploading",
            Queued => "queued",
            Transcribing => "transcribing",
            Done => "done",
            Silent => "silent",
            NoModel => "no-model",
            Failed => "failed",
            Cancelled => "cancelled",
        }
    }
}

/// A named thread of dictations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Monolog {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub created: String,
    /// How many messages it holds; filled in when the list is built.
    #[serde(default)]
    pub count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Stage; 9] = [
        Stage::Draft,
        Stage::Uploading,
        Stage::Queued,
        Stage::Transcribing,
        Stage::Done,
        Stage::Silent,
        Stage::NoModel,
        Stage::Failed,
        Stage::Cancelled,
    ];

    #[test]
    fn only_the_listed_transitions_are_allowed() {
        let mut allowed = 0;
        for from in ALL {
            for to in ALL {
                let listed = from.next().contains(&to);
                assert_eq!(
                    from.can_move_to(to),
                    listed,
                    "can_move_to disagrees with the table for {from:?} -> {to:?}"
                );
                // A stage never moves to itself: that would be a no-op dressed
                // as progress, and it would let an interface spin.
                assert!(
                    !from.can_move_to(from),
                    "{from:?} must not move to itself"
                );
                if listed {
                    allowed += 1;
                }
            }
        }
        // A sanity bound: the table is small, and every source stage may at
        // least be cancelled or fail.
        assert!(allowed >= 20, "the table lost transitions: {allowed}");
    }

    #[test]
    fn a_finished_message_never_moves_again() {
        for stage in [Stage::Done, Stage::Silent, Stage::Cancelled] {
            assert!(stage.is_final());
            assert!(
                stage.next().is_empty(),
                "{stage:?} must be a dead end"
            );
        }
    }

    #[test]
    fn work_in_flight_is_open() {
        for stage in [Stage::Draft, Stage::Uploading, Stage::Queued, Stage::Transcribing] {
            assert!(stage.is_open(), "{stage:?} has work to do");
        }
        for stage in [Stage::Done, Stage::Silent, Stage::NoModel, Stage::Failed, Stage::Cancelled]
        {
            assert!(!stage.is_open(), "{stage:?} has no work left");
        }
    }

    #[test]
    fn every_stage_has_its_own_word() {
        let mut words: Vec<&str> = ALL.iter().map(|stage| stage.label()).collect();
        words.sort_unstable();
        words.dedup();
        assert_eq!(words.len(), ALL.len(), "two stages share a label");
    }



}
