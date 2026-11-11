//! Joining the text of consecutive audio segments.
//!
//! Segments overlap when the cut was forced (continuous speech). With token
//! timestamps from the engine, every word is placed in exactly one segment by
//! the position of the middle of the word: the neighbouring segment decoded the
//! same word too, but its midpoint belongs to the other commit range, so the
//! duplicate is dropped by time rather than by comparing strings. Words cut by
//! the boundary are complete on both sides because of the 0.6 s overlap.
//!
//! Engines that report no timestamps fall back to a textual overlap merge.

#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub start: f32,
    pub end: f32,
    pub text: String,
}

impl Word {
    pub fn midpoint(&self) -> f32 {
        (self.start + self.end) / 2.0
    }
}

/// Group subword tokens into words. A token that starts with `▁` (U+2581) or a
/// space begins a new word; so does the first token after a token that is only a
/// space — the engine emits `" "` as a standalone boundary token.
pub fn words(tokens: &[String], times: &[f32]) -> Vec<Word> {
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Word> = Vec::new();
    let mut boundary = false;
    for (i, token) in tokens.iter().enumerate() {
        let time = times.get(i).copied().unwrap_or_else(|| {
            times.last().copied().unwrap_or(0.0)
        });
        let starts = token.starts_with('\u{2581}') || token.starts_with(' ');
        let text = token.replace('\u{2581}', " ").trim().to_string();
        if text.is_empty() {
            boundary = true;
            continue;
        }
        if starts || boundary || out.is_empty() {
            out.push(Word {
                start: time,
                end: time,
                text,
            });
        } else if let Some(last) = out.last_mut() {
            last.text.push_str(&text);
        }
        boundary = false;
    }
    // A word ends where the next one starts.
    for i in 0..out.len().saturating_sub(1) {
        let next = out[i + 1].start;
        if next > out[i].start {
            out[i].end = next;
        }
    }
    if let Some(last) = out.last_mut() {
        if last.end <= last.start {
            last.end = last.start + 0.2;
        }
    }
    out
}

/// Text stitched from segments, in order.
#[derive(Debug, Default)]
pub struct Stitcher {
    words: Vec<String>,
}

impl Stitcher {
    pub fn new() -> Self {
        Self { words: Vec::new() }
    }

    pub fn words(&self) -> &[String] {
        &self.words
    }

    pub fn text(&self) -> String {
        self.words.join(" ")
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Commit the words of one segment. `audio_start` is the absolute time of
    /// the segment's first sample; words are kept when their midpoint falls in
    /// `[commit_start, commit_end)`.
    pub fn commit(
        &mut self,
        tokens: &[String],
        times: &[f32],
        audio_start: f32,
        commit_start: f32,
        commit_end: f32,
    ) -> usize {
        if times.len() != tokens.len() || times.is_empty() {
            return self.push_text(&join_tokens(tokens));
        }
        let words = words(tokens, times);
        let mut kept: Vec<String> = Vec::new();
        for word in &words {
            let absolute = audio_start + word.midpoint();
            if absolute >= commit_start && absolute < commit_end {
                kept.push(word.text.clone());
            }
        }
        let added = kept.len();
        self.words.extend(kept);
        added
    }

    /// Append text without timestamps, removing the words that repeat the tail
    /// already committed (the overlap decoded twice). Text keeps its original
    /// spelling and punctuation; only the comparison is normalized.
    pub fn push_text(&mut self, text: &str) -> usize {
        let incoming: Vec<String> = text.split_whitespace().map(str::to_string).collect();
        if incoming.is_empty() {
            return 0;
        }
        let overlap = longest_overlap(&self.words, &incoming);
        let fresh = &incoming[overlap..];
        let added = fresh.len();
        self.words.extend(fresh.iter().cloned());
        added
    }
}

/// Join tokens the way the engines do: `▁` becomes a space.
pub fn join_tokens(tokens: &[String]) -> String {
    let mut out = String::new();
    for token in tokens {
        out.push_str(&token.replace('\u{2581}', " "));
    }
    collapse(&out)
}

pub fn collapse(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out
}

/// Words split for comparison: lowercased, punctuation dropped, `ё` folded.
pub fn normalize_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            for lower in c.to_lowercase() {
                current.push(if lower == 'ё' { 'е' } else { lower });
            }
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Length of the largest suffix of `committed` that is a prefix of `incoming`.
fn longest_overlap(committed: &[String], incoming: &[String]) -> usize {
    let tail: Vec<String> = committed
        .iter()
        .rev()
        .take(8)
        .map(|word| normalize_words(word).join(""))
        .collect();
    let head: Vec<String> = incoming
        .iter()
        .take(8)
        .map(|word| normalize_words(word).join(""))
        .collect();
    let max = tail.len().min(head.len());
    for size in (1..=max).rev() {
        // `tail` is reversed: the last `size` committed words in reading order.
        let suffix: Vec<&String> = (0..size).map(|i| &tail[size - 1 - i]).collect();
        if suffix.iter().zip(head.iter()).all(|(a, b)| *a == b) {
            return size;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::{words, Stitcher};

    fn tokens(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn subword_tokens_become_words() {
        let list = tokens(&["\u{2581}Прямо", "\u{2581}сейчас", "\u{2581}я"]);
        let grouped = words(&list, &[0.0, 0.5, 0.9]);
        assert_eq!(grouped.len(), 3);
        assert_eq!(grouped[0].text, "Прямо");
        assert_eq!(grouped[1].start, 0.5);
        assert_eq!(grouped[0].end, 0.5);
    }

    #[test]
    fn a_standalone_space_token_starts_a_new_word() {
        // The GigaAM transducer emits the boundary as its own " " token.
        let list = tokens(&["\u{2581}п", "ла", "ни", "ру", "ю", " ", "вести"]);
        let grouped = words(&list, &[0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].text, "планирую");
        assert_eq!(grouped[1].text, "вести");
        assert_eq!(grouped[1].start, 0.6);
    }

    #[test]
    fn continuous_tokens_extend_the_previous_word() {
        let list = tokens(&["\u{2581}при", "вет"]);
        let grouped = words(&list, &[1.0, 1.2]);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].text, "привет");
    }

    #[test]
    fn overlapping_segments_do_not_duplicate_words() {
        let mut stitcher = Stitcher::new();
        // The cut is at 1.5 s and "три" spans 1.4..1.6 s, so the middle of the
        // word is exactly on the boundary: segment A (audio from 0) must not
        // take it, segment B (audio from 0.9, the 0.6 s overlap) must.
        let a_tokens = tokens(&["\u{2581}раз", "\u{2581}два", "\u{2581}три"]);
        stitcher.commit(&a_tokens, &[0.2, 0.7, 1.4], 0.0, 0.0, 1.5);
        let b_tokens = tokens(&["\u{2581}три", "\u{2581}четыре"]);
        stitcher.commit(&b_tokens, &[0.5, 0.9], 0.9, 1.5, 3.0);
        assert_eq!(stitcher.text(), "раз два три четыре");
    }

    #[test]
    fn a_word_whose_middle_is_past_the_cut_belongs_to_the_next_segment() {
        let mut stitcher = Stitcher::new();
        // "слово" spans 1.9..2.3, midpoint 2.1 -> belongs to the second range.
        let tokens_a = tokens(&["\u{2581}до", "\u{2581}слово"]);
        stitcher.commit(&tokens_a, &[0.5, 1.3], 0.6, 1.0, 2.0);
        let tokens_b = tokens(&["\u{2581}слово", "\u{2581}после"]);
        stitcher.commit(&tokens_b, &[0.5, 1.2], 1.6, 2.0, 4.0);
        assert_eq!(stitcher.text(), "до слово после");
    }

    #[test]
    fn without_timestamps_the_textual_overlap_is_removed() {
        let mut stitcher = Stitcher::new();
        stitcher.push_text("раз два три");
        stitcher.push_text("три четыре пять");
        assert_eq!(stitcher.text(), "раз два три четыре пять");
    }

    #[test]
    fn a_fully_repeated_chunk_adds_nothing() {
        let mut stitcher = Stitcher::new();
        stitcher.push_text("раз два три");
        assert_eq!(stitcher.push_text("раз два три"), 0);
        assert_eq!(stitcher.text(), "раз два три");
    }

    #[test]
    fn punctuation_differences_do_not_break_the_overlap() {
        let mut stitcher = Stitcher::new();
        stitcher.push_text("Привет, мир!");
        stitcher.push_text("мир! Как дела");
        assert_eq!(stitcher.text(), "Привет, мир! Как дела");
    }
}
