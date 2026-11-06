use windows_sys::Win32::Media::Audio::{PlaySoundA, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

// Embedded in the binary so the exe stays self-contained.
const START_WAV: &[u8] = include_bytes!("../assets/start.wav");
const END_WAV: &[u8] = include_bytes!("../assets/end.wav");

/// PlaySound with SND_MEMORY requires the buffer to outlive the asynchronous
/// playback. Keep the current and the previous buffer alive; starting a new
/// sound stops the previous one, so the slot two generations back is always
/// safe to drop.
static PLAY_BUFFER: std::sync::Mutex<(Option<Box<[u8]>>, Option<Box<[u8]>>)> =
    std::sync::Mutex::new((None, None));

pub fn play_start(volume: f32) {
    play(START_WAV, volume);
}

pub fn play_end(volume: f32) {
    play(END_WAV, volume);
}

/// Scale the 16-bit PCM samples in a copy of the WAV (perceptual: volume²)
/// and play it. `volume` is 0.0..=1.0.
fn play(wav: &'static [u8], volume: f32) {
    let scaled = scale_pcm16(wav, volume.clamp(0.0, 1.0));
    let ptr = scaled.as_ptr();
    let mut guard = PLAY_BUFFER.lock().unwrap();
    guard.1 = guard.0.take();
    guard.0 = Some(scaled);
    // SAFETY: with SND_MEMORY, pszSound points at an in-memory WAV image. The
    // buffer is kept alive in PLAY_BUFFER, which outlives the playback.
    unsafe {
        PlaySoundA(ptr, std::ptr::null_mut(), SND_ASYNC | SND_MEMORY | SND_NODEFAULT);
    }
}

/// Find the data chunk of a RIFF/WAVE file and return the byte offset of its
/// payload.
fn data_chunk(wav: &[u8]) -> Option<usize> {
    let mut pos = 12; // skip RIFF header
    while pos + 8 <= wav.len() {
        let size = u32::from_le_bytes([wav[pos + 4], wav[pos + 5], wav[pos + 6], wav[pos + 7]])
            as usize;
        if &wav[pos..pos + 4] == b"data" {
            return Some(pos + 8);
        }
        pos += 8 + size + (size & 1);
    }
    None
}

fn scale_pcm16(wav: &[u8], volume: f32) -> Box<[u8]> {
    let mut out = wav.to_vec();
    let scale = (volume * volume) as f64;
    if let Some(start) = data_chunk(&out) {
        for chunk in out[start..].chunks_exact_mut(2) {
            let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
            let scaled =
                (sample as f64 * scale).round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
            chunk.copy_from_slice(&scaled.to_le_bytes());
        }
    }
    out.into_boxed_slice()
}
