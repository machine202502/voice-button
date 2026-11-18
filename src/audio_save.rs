//! Copies of finished recordings, saved where the user asks for them.
//!
//! The original audio is never moved or changed: the session, the phone layer
//! and the bot keep owning their own files. When a take ends, this module
//! encodes a copy as Ogg/Opus — the smallest format that keeps speech clean —
//! and leaves it in a folder tree that says where the take came from.

use crate::audio::Resampler;
use crate::config::Config;
use anyhow::{Context, Result};
use ogg::{PacketWriteEndInfo, PacketWriter};
use opus::{Application, Bitrate, Channels, Encoder};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Every recording the app makes is 16 kHz mono.
const RATE: u32 = 16_000;
/// One Opus frame: 20 ms, the size the voice mode wants.
const FRAME: usize = 320;
/// 24 kbit/s mono: about 11 MB an hour, which is transparent enough for speech.
const BITRATE: i32 = 24_000;
/// Destination when the user did not pick a folder: this name inside Music.
pub const DEFAULT_FOLDER: &str = "VoiceButton";

/// Where a take came from. Each one gets its own folder, so the tree mirrors
/// the app: a dictation, an imported file, the API, the phone and the bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    User,
    File,
    Api,
    Phone,
    Telegram,
}

impl Origin {
    fn folder(self) -> &'static str {
        match self {
            Origin::User => "user",
            Origin::File => "file",
            Origin::Api => "api",
            Origin::Phone => "phone",
            Origin::Telegram => "telegram",
        }
    }
}

/// What to write, and how the file should be named.
pub struct Recording<'a> {
    pub origin: Origin,
    /// Telegram sender: every conversation partner gets a folder of their own.
    /// Empty for every other origin.
    pub account: &'a str,
    /// Extra words for the file name: the imported file, the monolog, the
    /// media kind. Empty means "date only".
    pub label: &'a str,
    /// Local wall clock of the recording, e.g. `2026-11-18 12:16:51`.
    pub at: &'a str,
}

/// The folder saved recordings go to: the user's choice, or Music/VoiceButton.
pub fn folder(cfg: &Config) -> PathBuf {
    let custom = cfg.save_recordings_dir.trim();
    if !custom.is_empty() {
        return PathBuf::from(custom);
    }
    dirs::audio_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DEFAULT_FOLDER)
}

/// Save a copy of a 16-bit mono WAV (the session's own file).
pub fn save_wav(cfg: &Config, recording: &Recording<'_>, wav: &Path) -> Result<Option<PathBuf>> {
    if !cfg.save_recordings {
        return Ok(None);
    }
    let (mut source, rate) = PcmReader::open_wav(wav)?;
    encode(cfg, recording, rate, || source.next(4096))
}

/// Save a copy of the phone's raw PCM16: 16 kHz mono, as it arrived.
pub fn save_pcm16(cfg: &Config, recording: &Recording<'_>, pcm: &Path) -> Result<Option<PathBuf>> {
    if !cfg.save_recordings {
        return Ok(None);
    }
    let (mut source, rate) = PcmReader::open_raw(pcm, RATE)?;
    encode(cfg, recording, rate, || source.next(4096))
}

/// Save a copy of samples already decoded to 16 kHz mono (the Telegram job).
pub fn save_samples(
    cfg: &Config,
    recording: &Recording<'_>,
    samples: &[f32],
) -> Result<Option<PathBuf>> {
    if !cfg.save_recordings {
        return Ok(None);
    }
    let mut at = 0usize;
    encode(cfg, recording, RATE, || {
        if at >= samples.len() {
            return Ok(None);
        }
        let end = (at + 4096).min(samples.len());
        let chunk = samples[at..end].to_vec();
        at = end;
        Ok(Some(chunk))
    })
}

/// Shared pipeline: read chunks at `rate`, resample to 16 kHz and encode.
fn encode(
    cfg: &Config,
    recording: &Recording<'_>,
    rate: u32,
    mut next: impl FnMut() -> Result<Option<Vec<f32>>>,
) -> Result<Option<PathBuf>> {
    let (final_path, part) = destination(cfg, recording);
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let file = File::create(&part).with_context(|| format!("create {}", part.display()))?;
    let mut writer = OpusFile::new(final_path, part.clone(), file)?;
    let mut resampler = (rate != RATE).then(|| Resampler::new(rate, RATE));
    let result = (|| -> Result<()> {
        while let Some(chunk) = next()? {
            let mut converted = Vec::new();
            let samples = match resampler.as_mut() {
                Some(resampler) => {
                    resampler.process(&chunk, &mut converted);
                    &converted[..]
                }
                None => &chunk[..],
            };
            writer.write(samples)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => writer.finish(),
        Err(error) => {
            // A half-written file is not a recording: never leave it behind.
            let _ = std::fs::remove_file(&part);
            Err(error)
        }
    }
}

/// Whether a copy of this recording is already in the destination folder.
///
/// A take whose text failed is kept for the recovery, and it was saved when it
/// ended: the recovery must not add a second copy of the same recording.
pub fn already_saved(cfg: &Config, recording: &Recording<'_>) -> bool {
    let folder = origin_folder(cfg, recording);
    let stem = file_stem(recording);
    let Ok(entries) = std::fs::read_dir(&folder) else {
        return false;
    };
    let plain = format!("{stem}.ogg");
    let numbered = format!("{stem}-");
    entries.flatten().any(|entry| {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == plain {
            return true;
        }
        name.strip_prefix(&numbered)
            .and_then(|rest| rest.strip_suffix(".ogg"))
            .map(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
            .unwrap_or(false)
    })
}

/// A unique name in the destination folder: `<date>[_<label>].ogg`.
///
/// Two takes can start in the same second, so an existing name gets a counter
/// instead of being overwritten.
fn destination(cfg: &Config, recording: &Recording<'_>) -> (PathBuf, PathBuf) {
    let folder = origin_folder(cfg, recording);
    let stem = file_stem(recording);
    let mut candidate = folder.join(format!("{stem}.ogg"));
    let mut counter = 2;
    while candidate.exists() {
        candidate = folder.join(format!("{stem}-{counter}.ogg"));
        counter += 1;
    }
    let part = candidate.with_extension("part");
    (candidate, part)
}

/// The folder that holds the copies of one origin: the account folder for
/// Telegram, the origin folder for everything else.
fn origin_folder(cfg: &Config, recording: &Recording<'_>) -> PathBuf {
    let mut folder = folder(cfg);
    folder.push(recording.origin.folder());
    if recording.origin == Origin::Telegram {
        folder.push(safe_name(recording.account, "unknown"));
    }
    folder
}

/// `2026-11-18_12-16-51` plus the recording's own label, if it has one.
fn file_stem(recording: &Recording<'_>) -> String {
    let mut stem = stamp(recording.at);
    let label = safe_name(recording.label, "");
    if !label.is_empty() {
        stem.push('_');
        stem.push_str(&label);
    }
    stem
}

/// `2026-11-18 12:16:51` (or an RFC 3339 stamp) as `2026-11-18_12-16-51`.
fn stamp(at: &str) -> String {
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(at) {
        return time
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d_%H-%M-%S")
            .to_string();
    }
    if let Ok(time) = chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M:%S") {
        return time.format("%Y-%m-%d_%H-%M-%S").to_string();
    }
    chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string()
}

/// A Windows-safe file or folder name.
///
/// The characters Windows forbids become spaces, control characters and runs
/// of whitespace collapse into one space, and reserved device names get a
/// prefix so a monolog called "CON" still produces a file.
fn safe_name(text: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = false;
    for c in text.chars() {
        let c = if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
            ' '
        } else {
            c
        };
        if c == ' ' {
            if last_space || out.is_empty() {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        out.push(c);
    }
    let mut out: String = out.trim_end_matches([' ', '.']).chars().take(64).collect();
    out = out.trim_matches(' ').to_string();
    if out.is_empty() {
        return fallback.to_string();
    }
    let upper = out.to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|name| upper == *name)
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit());
    if reserved {
        out.insert(0, '_');
    }
    out
}

/// An Ogg/Opus writer that packs 20 ms frames into pages.
struct OpusFile {
    writer: PacketWriter<'static, BufWriter<File>>,
    encoder: Encoder,
    frame: Vec<f32>,
    /// The last encoded packet waits here, so the final one can carry the
    /// end-of-stream flag without a second pass.
    pending: Option<(Vec<u8>, u64)>,
    /// Granule position in 48 kHz units, as Ogg/Opus requires.
    granule: u64,
    serial: u32,
    part: PathBuf,
    final_path: PathBuf,
}

impl OpusFile {
    fn new(final_path: PathBuf, part: PathBuf, file: File) -> Result<Self> {
        let mut encoder = Encoder::new(RATE, Channels::Mono, Application::Voip)
            .context("create the Opus encoder")?;
        // The encoder's own defaults are close; these make the copies small and
        // keep the voice mode's speech tuning.
        let _ = encoder.set_bitrate(Bitrate::Bits(BITRATE));
        let _ = encoder.set_signal(opus::Signal::Voice);
        let _ = encoder.set_vbr(true);
        let serial = serial_number();
        let mut writer = PacketWriter::new(BufWriter::new(file));
        writer
            .write_packet(head(), serial, PacketWriteEndInfo::EndPage, 0)
            .context("write the Opus header")?;
        writer
            .write_packet(tags(), serial, PacketWriteEndInfo::EndPage, 0)
            .context("write the Opus tags")?;
        Ok(Self {
            writer,
            encoder,
            frame: Vec::with_capacity(FRAME),
            pending: None,
            granule: 0,
            serial,
            part,
            final_path,
        })
    }

    fn write(&mut self, samples: &[f32]) -> Result<()> {
        for &sample in samples {
            self.frame.push(sample);
            if self.frame.len() == FRAME {
                let packet = self.encoder.encode_vec_float(&self.frame, 4000)?;
                self.frame.clear();
                self.granule += FRAME as u64 * 3;
                self.push(packet)?;
            }
        }
        Ok(())
    }

    fn push(&mut self, packet: Vec<u8>) -> Result<()> {
        if let Some((previous, granule)) = self.pending.replace((packet, self.granule)) {
            self.writer
                .write_packet(previous, self.serial, PacketWriteEndInfo::NormalPacket, granule)?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Option<PathBuf>> {
        if !self.frame.is_empty() {
            // The last frame is padded with silence and the granule position
            // trims it back: no decoder has to guess where the take ended.
            let real = self.frame.len() as u64;
            self.frame.resize(FRAME, 0.0);
            let packet = self.encoder.encode_vec_float(&self.frame, 4000)?;
            self.granule += real * 3;
            self.push(packet)?;
        }
        let Some((last, granule)) = self.pending.take() else {
            // Nothing arrived: no empty file is written.
            let _ = std::fs::remove_file(&self.part);
            return Ok(None);
        };
        self.writer
            .write_packet(last, self.serial, PacketWriteEndInfo::EndStream, granule)
            .context("write the last Opus packet")?;
        let mut buffered = self.writer.into_inner();
        buffered.flush()?;
        let file = buffered
            .into_inner()
            .map_err(|error| anyhow::anyhow!("flush {}: {error}", self.part.display()))?;
        file.sync_all().ok();
        drop(file);
        std::fs::rename(&self.part, &self.final_path).with_context(|| {
            format!(
                "rename {} to {}",
                self.part.display(),
                self.final_path.display()
            )
        })?;
        Ok(Some(self.final_path))
    }
}

/// `OpusHead`: version 1, mono, 16 kHz input, no pre-skip and no gain.
fn head() -> Vec<u8> {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(1); // channels
    head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
    head.extend_from_slice(&RATE.to_le_bytes()); // input sample rate
    head.extend_from_slice(&0i16.to_le_bytes()); // output gain
    head.push(0); // channel mapping family
    head
}

/// `OpusTags`: the vendor string and no comments.
fn tags() -> Vec<u8> {
    let vendor = b"voice-button";
    let mut tags = Vec::with_capacity(8 + 4 + vendor.len() + 4);
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes());
    tags
}

/// A serial that only has to differ from other streams in the same file.
fn serial_number() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    (nanos ^ (std::process::id() as u64) << 32) as u32
}

/// Chunked reader for 16-bit PCM: a long file never has to fit in memory.
struct PcmReader {
    file: BufReader<File>,
    channels: usize,
    remaining: u64,
}

impl PcmReader {
    fn open_wav(path: &Path) -> Result<(Self, u32)> {
        let mut file = BufReader::new(
            File::open(path).with_context(|| format!("open {}", path.display()))?,
        );
        let mut riff = [0u8; 12];
        file.read_exact(&mut riff)
            .with_context(|| format!("read {}", path.display()))?;
        if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
            anyhow::bail!("{} is not a RIFF/WAVE file", path.display());
        }
        let mut rate = RATE;
        let mut channels = 1u16;
        loop {
            let mut chunk = [0u8; 8];
            file.read_exact(&mut chunk)
                .with_context(|| format!("read {}", path.display()))?;
            let size =
                u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
            if &chunk[0..4] == b"fmt " {
                let mut format = vec![0u8; size as usize];
                file.read_exact(&mut format)?;
                if format.len() < 16
                    || u16::from_le_bytes([format[0], format[1]]) != 1
                    || u16::from_le_bytes([format[14], format[15]]) != 16
                {
                    anyhow::bail!("{} is not 16-bit PCM", path.display());
                }
                channels = u16::from_le_bytes([format[2], format[3]]).max(1);
                rate = u32::from_le_bytes([format[4], format[5], format[6], format[7]]);
                if size % 2 == 1 {
                    file.seek(SeekFrom::Current(1))?;
                }
            } else if &chunk[0..4] == b"data" {
                let at = file.stream_position()?;
                let length = std::fs::metadata(path)?.len();
                let available = length.saturating_sub(at);
                return Ok((
                    Self {
                        file,
                        channels: channels as usize,
                        remaining: size.min(available),
                    },
                    rate,
                ));
            } else {
                file.seek(SeekFrom::Current((size + (size % 2)) as i64))?;
            }
        }
    }

    fn open_raw(path: &Path, rate: u32) -> Result<(Self, u32)> {
        let file = BufReader::new(
            File::open(path).with_context(|| format!("open {}", path.display()))?,
        );
        let remaining = std::fs::metadata(path)?.len();
        Ok((
            Self {
                file,
                channels: 1,
                remaining,
            },
            rate,
        ))
    }

    fn next(&mut self, frames: usize) -> Result<Option<Vec<f32>>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let frame_bytes = 2 * self.channels as u64;
        let wanted = (frames as u64 * frame_bytes).min(self.remaining);
        let mut bytes = vec![0u8; wanted as usize];
        self.file.read_exact(&mut bytes)?;
        self.remaining -= wanted;
        let channels = self.channels;
        let samples = bytes
            .chunks_exact(2 * channels)
            .map(|frame| {
                let mut sum = 0.0f32;
                for channel in 0..channels {
                    let at = channel * 2;
                    sum += i16::from_le_bytes([frame[at], frame[at + 1]]) as f32 / 32768.0;
                }
                sum / channels as f32
            })
            .collect();
        Ok(Some(samples))
    }
}

#[cfg(test)]
mod tests {
    use super::{already_saved, folder, save_samples, save_wav, Origin, Recording, DEFAULT_FOLDER};
    use crate::audio::WavWriter;
    use crate::config::Config;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vb-save-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn config(dir: &std::path::Path) -> Config {
        let mut cfg = Config::default();
        cfg.save_recordings = true;
        cfg.save_recordings_dir = dir.display().to_string();
        cfg
    }

    fn tone(seconds: f32) -> Vec<f32> {
        (0..(16_000.0 * seconds) as usize)
            .map(|i| (i as f32 * 0.02).sin() * 0.4)
            .collect()
    }

    #[test]
    fn a_recording_is_saved_as_a_decodable_ogg_opus_file() {
        let dir = temp_dir("decode");
        let samples = tone(1.5);
        let recording = Recording {
            origin: Origin::User,
            account: "",
            label: "",
            at: "2026-11-18 12:16:51",
        };
        let path = save_samples(&config(&dir), &recording, &samples)
            .expect("save")
            .expect("a file");
        assert!(path.to_string_lossy().ends_with(".ogg"));
        assert!(path.file_name().unwrap().to_string_lossy().starts_with("2026-11-18_12-16-51"));

        let decoded = crate::telegram_media::decode_to_16k(&path).expect("decode the copy");
        let seconds = decoded.len() as f32 / 16_000.0;
        assert!(
            (seconds - 1.5).abs() < 0.05,
            "the copy is {seconds} s instead of 1.5 s"
        );
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(
            size < 20_000,
            "a one and a half second copy is {size} bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_default_folder_is_the_one_under_music() {
        let cfg = Config::default();
        assert_eq!(folder(&cfg), dirs::audio_dir().unwrap().join(DEFAULT_FOLDER));
        let mut cfg = cfg;
        cfg.save_recordings_dir = r"D:\Audio".to_string();
        assert_eq!(folder(&cfg), std::path::PathBuf::from(r"D:\Audio"));
    }

    #[test]
    fn every_origin_gets_its_own_folder_and_telegram_an_account() {
        let dir = temp_dir("groups");
        let cfg = config(&dir);
        for (origin, expected) in [
            (Origin::User, "user"),
            (Origin::File, "file"),
            (Origin::Api, "api"),
            (Origin::Phone, "phone"),
        ] {
            let recording = Recording {
                origin,
                account: "",
                label: "",
                at: "2026-11-18 12:16:51",
            };
            let path = save_samples(&cfg, &recording, &tone(0.2)).unwrap().unwrap();
            assert_eq!(
                path.parent().unwrap().strip_prefix(&dir).unwrap(),
                std::path::Path::new(expected)
            );
        }
        let recording = Recording {
            origin: Origin::Telegram,
            account: "Иван Петров",
            label: "voice 42",
            at: "2026-11-18 12:16:51",
        };
        let path = save_samples(&cfg, &recording, &tone(0.2)).unwrap().unwrap();
        assert_eq!(
            path.parent().unwrap().strip_prefix(&dir).unwrap(),
            std::path::Path::new("telegram").join("Иван Петров")
        );
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "2026-11-18_12-16-51_voice 42.ogg"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wav_recording_is_copied_and_the_original_stays() {
        let dir = temp_dir("wav");
        let source_dir = temp_dir("wav-source");
        let source = source_dir.join("audio.wav");
        let mut writer = WavWriter::create(&source, 16_000).unwrap();
        writer.write(&tone(0.5)).unwrap();
        writer.finish().unwrap();
        let recording = Recording {
            origin: Origin::File,
            account: "",
            label: "meeting.mp3",
            at: "2026-11-18 12:16:51",
        };
        let path = save_wav(&config(&dir), &recording, &source)
            .unwrap()
            .expect("a file");
        assert!(path.exists());
        assert!(source.exists(), "the original is left alone");
        assert!(path.to_string_lossy().contains("meeting.mp3"));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&source_dir);
    }

    #[test]
    fn saving_is_off_until_the_user_asks_for_it() {
        let dir = temp_dir("off");
        let cfg = Config::default();
        let recording = Recording {
            origin: Origin::User,
            account: "",
            label: "",
            at: "2026-11-18 12:16:51",
        };
        assert!(save_samples(&cfg, &recording, &tone(0.3)).unwrap().is_none());
        let mut cfg = cfg;
        cfg.save_recordings_dir = dir.display().to_string();
        assert!(save_samples(&cfg, &recording, &tone(0.3)).unwrap().is_none());
        assert!(!dir.join("user").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_takes_in_the_same_second_get_different_names() {
        let dir = temp_dir("twice");
        let cfg = config(&dir);
        let recording = Recording {
            origin: Origin::User,
            account: "",
            label: "",
            at: "2026-11-18 12:16:51",
        };
        let first = save_samples(&cfg, &recording, &tone(0.2)).unwrap().unwrap();
        let second = save_samples(&cfg, &recording, &tone(0.2)).unwrap().unwrap();
        assert_ne!(first, second);
        assert!(second.to_string_lossy().contains("-2.ogg"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_saved_copy_is_recognised_so_recovery_does_not_duplicate_it() {
        let dir = temp_dir("recognise");
        let cfg = config(&dir);
        let recording = Recording {
            origin: Origin::Telegram,
            account: "Anna",
            label: "voice 7",
            at: "2026-11-18 12:16:51",
        };
        assert!(!already_saved(&cfg, &recording));
        save_samples(&cfg, &recording, &tone(0.2)).unwrap().unwrap();
        assert!(already_saved(&cfg, &recording));
        // Another take at another second is a different recording.
        let other = Recording {
            at: "2026-11-18 12:17:10",
            ..recording
        };
        assert!(!already_saved(&cfg, &other));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsafe_names_are_made_windows_safe() {
        let dir = temp_dir("names");
        let recording = Recording {
            origin: Origin::Telegram,
            account: "a/b:c*?",
            label: "CON",
            at: "2026-11-18 12:16:51",
        };
        let path = save_samples(&config(&dir), &recording, &tone(0.2))
            .unwrap()
            .unwrap();
        let folder = path.parent().unwrap().file_name().unwrap().to_string_lossy();
        assert_eq!(folder, "a b c");
        assert!(path.file_name().unwrap().to_string_lossy().ends_with("_CON.ogg"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
