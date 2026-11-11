//! Silence-aware segmentation of a live 16 kHz stream.
//!
//! The recognizer used by the app cannot take an arbitrarily long utterance
//! (the GigaAM encoder is built for 5000 frames, 200 s) and re-decoding the
//! whole buffer every second costs O(n²) CPU. So the stream is cut into
//! segments:
//!
//! * a pause in speech ends a segment in the middle of the silence;
//! * speech that never pauses is cut at its quietest point near the target
//!   length, with an overlap so no word is lost between the two halves.
//!
//! Segments that contain almost no speech are dropped without ever reaching
//! the engine.

/// Samples per analysis frame (20 ms at 16 kHz).
const FRAME_MS: u32 = 20;
/// Audio kept before the first speech frame of a segment.
const PRE_ROLL: f32 = 0.25;
/// Padding around a cut made inside a pause (silence, so no word is affected).
const PAUSE_PAD: f32 = 0.15;
/// Overlap around a cut made in continuous speech. 0.6 s per side means a word
/// shorter than 1.2 s is complete in at least one of the two segments.
pub const FORCED_OVERLAP: f32 = 0.6;
/// Silence that ends a segment.
pub const PAUSE_CUT: f32 = 0.45;
/// Target length of a segment in continuous speech…
pub const TARGET_SEGMENT: f32 = 25.0;
/// …and the hard limit, past which the cut happens regardless of energy.
pub const MAX_SEGMENT: f32 = 30.0;
/// Speech shorter than this is dropped instead of transcribed.
pub const MIN_SPEECH: f32 = 0.35;
/// A pause only ends a segment once it is at least this long: shorter segments
/// give the recognizer too little context and it starts joining words.
pub const MIN_SEGMENT: f32 = 2.0;
/// Frames above the threshold that start a segment.
const START_FRAMES: usize = 3;

#[derive(Debug, Clone)]
pub struct Segment {
    pub index: usize,
    /// Audio range fed to the engine (includes the pads/overlap).
    pub audio_start: f32,
    pub audio_end: f32,
    /// Range of this segment's text: `[commit_start, commit_end)`.
    pub commit_start: f32,
    pub commit_end: f32,
    /// True when the cut landed in continuous speech and the neighbouring
    /// segments share audio.
    pub forced: bool,
    pub speech_seconds: f32,
    pub samples: Vec<f32>,
}

pub struct Segmenter {
    rate: u32,
    frame: usize,
    /// Desired segment length (seconds); a model may require less.
    target: f32,
    /// Ceiling after which a cut happens regardless of the target.
    hard: f32,
    /// Voice-activity sensitivity: >1 treats quieter speech as speech.
    sensitivity: f32,
    buf: Vec<f32>,
    /// Absolute sample index of `buf[0]`.
    base: u64,
    /// Frames analysed so far (samples relative to `base`).
    analysed: usize,
    floor: f32,
    in_speech: bool,
    speech_run: usize,
    silence_run: usize,
    seg_audio_start: f64,
    seg_commit_start: f64,
    resume_audio_start: f64,
    voiced: f64,
    last_voice_end: f64,
    index: usize,
    /// Recent frame energies for choosing a forced cut point.
    energies: std::collections::VecDeque<(f64, f32)>,
}

impl Segmenter {
    pub fn new(rate: u32) -> Self {
        Self::configured(rate, TARGET_SEGMENT, MAX_SEGMENT, 1.0)
    }

    /// Segmenter tuned for a model: 		arget is the preferred segment length,
    /// hard the ceiling, sensitivity scales the voice-activity threshold.
    pub fn configured(rate: u32, target: f32, hard: f32, sensitivity: f32) -> Self {
        let frame = (rate as f32 * FRAME_MS as f32 / 1000.0).round() as usize;
        let target = target.clamp(2.0, 120.0);
        Self {
            rate,
            frame: frame.max(1),
            target,
            hard: hard.max(target),
            sensitivity: sensitivity.clamp(0.25, 4.0),
            buf: Vec::new(),
            base: 0,
            analysed: 0,
            floor: 0.0015,
            in_speech: false,
            speech_run: 0,
            silence_run: 0,
            seg_audio_start: 0.0,
            seg_commit_start: 0.0,
            resume_audio_start: 0.0,
            voiced: 0.0,
            last_voice_end: 0.0,
            index: 0,
            energies: std::collections::VecDeque::new(),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// End of the stream in seconds (audio pushed so far).
    pub fn stream_end(&self) -> f32 {
        (self.base + self.buf.len() as u64) as f32 / self.rate as f32
    }

    pub fn push(&mut self, samples: &[f32]) -> Vec<Segment> {
        self.buf.extend_from_slice(samples);
        let mut out = Vec::new();
        while self.analysed + self.frame <= self.buf.len() {
            let start = self.analysed;
            let abs = self.base + start as u64;
            let rms = rms(&self.buf[start..start + self.frame]);
            self.step(abs, rms, &mut out);
            self.analysed += self.frame;
        }
        self.trim();
        out
    }

    /// Emit the tail segment when the recording stops.
    pub fn flush(&mut self) -> Option<Segment> {
        if !self.in_speech {
            return None;
        }
        let end = self.last_voice_end + PAUSE_PAD as f64 * self.rate as f64;
        let segment = self.emit(self.seg_commit_start, self.last_voice_end, end, false);
        self.in_speech = false;
        self.speech_run = 0;
        self.silence_run = 0;
        self.voiced = 0.0;
        segment
    }

    fn threshold(&self) -> f32 {
        ((self.floor * 3.0).max(0.0015)) / self.sensitivity
    }

    fn step(&mut self, abs: u64, rms: f32, out: &mut Vec<Segment>) {
        // Noise floor: fall immediately and rise only from frames that are
        // already below the threshold — otherwise continuous speech would drag
        // the floor up and the detector would declare the speaker silent in the
        // middle of a sentence.
        if rms < self.floor {
            self.floor = self.floor * 0.6 + rms * 0.4;
        } else if rms < self.threshold() {
            self.floor = self.floor * 0.995 + rms * 0.005;
        }
        let frame = self.frame as f64;
        let rate = self.rate as f64;
        let speaking = rms > self.threshold();

        self.energies.push_back((abs as f64, rms));
        if self.energies.len() > 400 {
            self.energies.pop_front();
        }

        if !self.in_speech {
            if speaking {
                self.speech_run += 1;
            } else {
                self.speech_run = 0;
            }
            if self.speech_run >= START_FRAMES {
                let speech_begin = abs as f64 - (self.speech_run as f64 - 1.0) * frame;
                self.in_speech = true;
                self.silence_run = 0;
                self.seg_commit_start = speech_begin;
                self.seg_audio_start = self
                    .resume_audio_start
                    .min(speech_begin - PRE_ROLL as f64 * rate)
                    .max(0.0);
                self.voiced = self.speech_run as f64 * frame / rate;
                self.last_voice_end = abs as f64 + frame;
            }
            return;
        }

        if speaking {
            self.silence_run = 0;
            self.voiced += frame / rate;
            self.last_voice_end = abs as f64 + frame;
        } else {
            self.silence_run += 1;
        }

        let now_end = abs as f64 + frame;
        let silence = self.silence_run as f64 * frame;
        let pause_cut = PAUSE_CUT as f64 * rate;

        if silence >= pause_cut {
            let silence_begin = now_end - silence;
            let cut = silence_begin + silence / 2.0;
            let spoken = (cut - self.seg_commit_start) / rate;
            if spoken < MIN_SEGMENT as f64 {
                return;
            }
            if let Some(segment) = self.emit(self.seg_commit_start, cut, cut + PAUSE_PAD as f64 * rate, false)
            {
                out.push(segment);
            }
            self.in_speech = false;
            self.speech_run = 0;
            self.silence_run = 0;
            self.voiced = 0.0;
            self.resume_audio_start = (cut - PAUSE_PAD as f64 * rate).max(0.0);
            return;
        }

        let length = (now_end - self.seg_commit_start) / rate;
        let target = if length >= self.hard as f64 {
            Some(self.target.min(self.hard))
        } else if length >= self.target as f64 {
            Some(self.target)
        } else {
            None
        };
        if let Some(target) = target {
            if self.voiced >= MIN_SPEECH as f64 {
                let cut = self.quietest_point(
                    (self.seg_commit_start + (target as f64 - 3.0).max(0.0) * rate) as u64,
                    (self.seg_commit_start + target as f64 * rate) as u64,
                );
                let cut = cut.max(self.seg_commit_start + 1.0 * rate);
                if let Some(segment) =
                    self.emit(self.seg_commit_start, cut, cut + FORCED_OVERLAP as f64 * rate, true)
                {
                    out.push(segment);
                }
                self.seg_commit_start = cut;
                self.seg_audio_start = (cut - FORCED_OVERLAP as f64 * rate).max(0.0);
                self.resume_audio_start = self.seg_audio_start;
                self.voiced = 0.0;
                self.last_voice_end = now_end;
            }
        }
    }

    /// Absolute sample index of the quietest frame in `[from, to)`.
    fn quietest_point(&self, from: u64, to: u64) -> f64 {
        let mut best = (from as f64, f32::MAX);
        for (abs, energy) in &self.energies {
            if *abs >= from as f64 && *abs < to as f64 && *energy < best.1 {
                best = (*abs, *energy);
            }
        }
        if best.1 == f32::MAX {
            to as f64
        } else {
            best.0
        }
    }

    fn emit(&mut self, commit_start: f64, commit_end: f64, audio_end: f64, forced: bool) -> Option<Segment> {
        let speech = self.voiced;
        if speech < MIN_SPEECH as f64 {
            return None;
        }
        let audio_start = self.seg_audio_start.max(0.0);
        let audio_end = audio_end.max(audio_start);
        let from = ((audio_start as u64).saturating_sub(self.base)) as usize;
        let to = (((audio_end as u64).saturating_sub(self.base)) as usize).min(self.buf.len());
        if to <= from {
            return None;
        }
        let index = self.index;
        self.index += 1;
        Some(Segment {
            index,
            audio_start: (self.base + from as u64) as f32 / self.rate as f32,
            audio_end: (self.base + to as u64) as f32 / self.rate as f32,
            commit_start: commit_start as f32 / self.rate as f32,
            commit_end: commit_end as f32 / self.rate as f32,
            forced,
            speech_seconds: speech as f32,
            samples: self.buf[from..to].to_vec(),
        })
    }

    /// Drop audio that no pending segment can use.
    fn trim(&mut self) {
        // A segment is at most `MAX_SEGMENT` long, so anything older than a
        // generous multiple of it can never be decoded again. This also bounds
        // the buffer of a recording that stays silent for a long time.
        let now = self.base + self.analysed as u64;
        let horizon = (self.hard as f64 * 1.5 * self.rate as f64) as u64;
        let floor = now.saturating_sub(horizon);
        let keep = (self.resume_audio_start.max(0.0) as u64)
            .max(floor)
            .min(now);
        let drop = keep.saturating_sub(self.base) as usize;
        if drop > 1_000 {
            self.buf.drain(..drop);
            self.base += drop as u64;
            self.analysed = self.analysed.saturating_sub(drop);
        }
    }
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::{Segmenter, MAX_SEGMENT, MIN_SPEECH, PAUSE_CUT, TARGET_SEGMENT};

    const RATE: u32 = 16_000;

    fn tone(seconds: f32, amplitude: f32) -> Vec<f32> {
        (0..(seconds * RATE as f32) as usize)
            .map(|i| (i as f32 * 0.08).sin() * amplitude)
            .collect()
    }

    fn silence(seconds: f32) -> Vec<f32> {
        vec![0.0; (seconds * RATE as f32) as usize]
    }

    #[test]
    fn silence_produces_no_segments() {
        let mut segmenter = Segmenter::new(RATE);
        let mut out = segmenter.push(&silence(5.0));
        out.extend(segmenter.push(&silence(5.0)));
        assert!(out.is_empty());
        assert!(segmenter.flush().is_none());
    }

    #[test]
    fn a_pause_ends_a_segment() {
        let mut segmenter = Segmenter::new(RATE);
        let mut out = segmenter.push(&tone(2.5, 0.3));
        out.extend(segmenter.push(&silence(PAUSE_CUT + 0.5)));
        assert_eq!(out.len(), 1, "expected one segment at the pause");
        let segment = &out[0];
        assert!(!segment.forced);
        assert!(segment.speech_seconds >= 2.4);
        assert!(segment.commit_end > segment.commit_start);
        // The cut lands in the silence, so the padded audio ends inside it.
        assert!(segment.audio_end > 2.4);
    }

    #[test]
    fn short_blips_are_dropped() {
        let mut segmenter = Segmenter::new(RATE);
        let mut out = segmenter.push(&tone(0.1, 0.3));
        out.extend(segmenter.push(&silence(1.0)));
        assert!(out.is_empty());
        assert!(MIN_SPEECH > 0.1);
    }

    #[test]
    fn continuous_speech_is_cut_at_the_target() {
        let mut segmenter = Segmenter::new(RATE);
        let mut out = Vec::new();
        let mut spoken = 0.0;
        while spoken < TARGET_SEGMENT * 2.0 + 5.0 {
            out.extend(segmenter.push(&tone(1.0, 0.3)));
            spoken += 1.0;
        }
        assert!(out.len() >= 2, "expected forced cuts, got {}", out.len());
        let first = &out[0];
        assert!(first.forced);
        assert!(first.commit_end - first.commit_start > 20.0);
        // Consecutive commit ranges are contiguous and the audio overlaps.
        assert!((out[1].commit_start - out[0].commit_end).abs() < 0.05);
        assert!(out[1].audio_start < out[0].audio_end);
    }

    #[test]
    fn flush_returns_the_tail() {
        let mut segmenter = Segmenter::new(RATE);
        let mut out = segmenter.push(&tone(3.0, 0.3));
        let tail = segmenter.flush();
        assert!(tail.is_some(), "tail segment expected");
        out.extend(tail);
        assert_eq!(out.len(), 1);
        assert!(out[0].speech_seconds > 2.5);
    }

    #[test]
    fn ten_minutes_of_speech_never_produces_a_long_segment() {
        let mut segmenter = Segmenter::new(RATE);
        let mut segments = Vec::new();
        for _ in 0..600 {
            segments.extend(segmenter.push(&tone(1.0, 0.3)));
        }
        if let Some(tail) = segmenter.flush() {
            segments.push(tail);
        }
        assert!(
            segments.len() > 15,
            "expected many segments, got {}",
            segments.len()
        );
        for segment in &segments {
            let audio = segment.audio_end - segment.audio_start;
            assert!(
                audio <= super::MAX_SEGMENT,
                "segment {} covers {:.1}s of audio",
                segment.index,
                audio
            );
            assert!(segment.speech_seconds >= super::MIN_SPEECH);
        }
    }

    #[test]
    fn a_long_silence_does_not_grow_the_buffer() {
        let mut segmenter = Segmenter::new(RATE);
        for _ in 0..180 {
            assert!(segmenter.push(&silence(1.0)).is_empty());
        }
        assert!(
            segmenter.buf.len() < (super::MAX_SEGMENT as usize + 20) * RATE as usize,
            "the buffer grew to {} samples while silent",
            segmenter.buf.len()
        );
    }

    #[test]
    fn sensitivity_makes_quiet_speech_count_as_speech() {
        // A tone well below the default threshold: ignored at 100 %…
        let quiet = tone(3.0, 0.0009);
        let mut strict = Segmenter::configured(RATE, TARGET_SEGMENT, MAX_SEGMENT, 1.0);
        let mut strict_out = strict.push(&quiet);
        strict_out.extend(strict.push(&silence(1.0)));
        assert!(strict_out.is_empty(), "quiet audio must not start a segment");

        // …and picked up at 300 %.
        let mut sensitive = Segmenter::configured(RATE, TARGET_SEGMENT, MAX_SEGMENT, 3.0);
        let mut loud_out = sensitive.push(&quiet);
        loud_out.extend(sensitive.push(&silence(1.0)));
        assert_eq!(loud_out.len(), 1, "sensitive detection must produce a segment");
    }

    #[test]
    fn a_model_limit_shortens_segments() {
        // A model that accepts only five seconds must not receive more.
        let mut segmenter = Segmenter::configured(RATE, 5.0, 6.0, 1.0);
        let mut out = Vec::new();
        for _ in 0..12 {
            out.extend(segmenter.push(&tone(1.0, 0.3)));
        }
        assert!(out.len() >= 2, "expected cuts at five seconds, got {}", out.len());
        for segment in &out {
            assert!(
                segment.audio_end - segment.audio_start <= 6.5,
                "segment {} is {:.1}s",
                segment.index,
                segment.audio_end - segment.audio_start
            );
        }
    }

    #[test]
    fn audio_is_trimmed_while_recording() {
        let mut segmenter = Segmenter::new(RATE);
        for _ in 0..40 {
            segmenter.push(&tone(1.0, 0.3));
        }
        // Roughly one segment of audio plus a small margin, not the whole 40 s.
        assert!(
            segmenter.buf.len() < (TARGET_SEGMENT as usize + 12) * RATE as usize,
            "buffer grew to {} samples",
            segmenter.buf.len()
        );
    }
}
