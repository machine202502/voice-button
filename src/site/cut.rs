//! The real cutter: the app's speech splitter behind the queue's `Cutter`.
//!
//! The queue only needs "here is audio, give me pieces worth decoding". The
//! splitter that decides where a sentence ends lives in `segmenter`; this is
//! the two-line bridge between them, kept separate so the boundary can be tested
//! with real audio instead of being buried inside the hub.

use super::hub::Cutter;
use super::queue::Segment;

pub struct RealCutter {
    inner: crate::segmenter::Segmenter,
    index: u32,
}

impl RealCutter {
    /// `target` is the wanted segment length in seconds, `hard` the ceiling for
    /// speech that never pauses.
    pub fn new(target: f32, hard: f32, sensitivity: f32) -> Self {
        Self {
            inner: crate::segmenter::Segmenter::configured(
                crate::session::SAMPLE_RATE,
                target,
                hard,
                sensitivity,
            ),
            index: 0,
        }
    }

    fn wrap(&mut self, source: crate::segmenter::Segment) -> Segment {
        let segment = Segment {
            index: self.index,
            samples: source.samples,
        };
        self.index += 1;
        segment
    }
}

impl Cutter for RealCutter {
    fn push(&mut self, samples: &[f32]) -> Vec<Segment> {
        let pieces = self.inner.push(samples);
        pieces.into_iter().map(|piece| self.wrap(piece)).collect()
    }

    fn flush(&mut self) -> Option<Segment> {
        self.inner.flush().map(|piece| self.wrap(piece))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: usize = 16_000;

    fn silence(seconds: f32) -> Vec<f32> {
        vec![0.0; (RATE as f32 * seconds) as usize]
    }

    /// A tone loud enough to look like speech to the voice detector.
    fn speech(seconds: f32) -> Vec<f32> {
        let count = (RATE as f32 * seconds) as usize;
        (0..count)
            .map(|index| {
                let t = index as f32 / RATE as f32;
                (t * 220.0 * std::f32::consts::TAU).sin() * 0.4
            })
            .collect()
    }

    #[test]
    fn silence_alone_produces_nothing_to_decode() {
        let mut cutter = RealCutter::new(8.0, 10.0, 1.0);
        let pieces = cutter.push(&silence(5.0));
        assert!(pieces.is_empty(), "there is no speech to cut");
        assert!(cutter.flush().is_none(), "and nothing to flush");
    }

    #[test]
    fn long_speech_is_cut_into_segments() {
        let mut cutter = RealCutter::new(4.0, 5.0, 1.0);
        let mut pieces = Vec::new();
        // Twelve seconds of continuous speech: the target is four seconds, so
        // at least one cut must land before the end.
        for _ in 0..12 {
            pieces.extend(cutter.push(&speech(1.0)));
        }
        assert!(!pieces.is_empty(), "speech must be cut into pieces");
        assert!(
            pieces[0].samples.len() > RATE,
            "a segment carries real audio, not an empty frame"
        );
        let indexes: Vec<u32> = pieces.iter().map(|piece| piece.index).collect();
        let mut sorted = indexes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), indexes.len(), "segment numbers never repeat");
    }

    #[test]
    fn the_tail_is_handed_over_when_the_phone_stops() {
        let mut cutter = RealCutter::new(8.0, 10.0, 1.0);
        // Two seconds of speech: too short to be cut on its own.
        assert!(cutter.push(&speech(2.0)).is_empty());
        let tail = cutter.flush().expect("the tail is not lost");
        assert!(tail.samples.len() > RATE / 2);
    }
}
