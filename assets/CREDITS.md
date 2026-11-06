# Bundled sound cues

Two short recordings, both released under **CC0 1.0** (public domain dedication), so they can be
shipped inside this extension without attribution requirements. Credits are given as courtesy.

| File | Used for | Source | Author | License |
| --- | --- | --- | --- | --- |
| `start.wav` | start of recording | [Simple Happy Beep.ogg](https://commons.wikimedia.org/wiki/File:Simple_Happy_Beep.ogg) — via [freesound.org/s/518147](https://freesound.org/people/waveplaySFX/sounds/518147/) | waveplaySFX | CC0 1.0 |
| `end.wav` | end of recording | [Nudge.ogg](https://commons.wikimedia.org/wiki/File:Nudge.ogg) | Wikimedia Commons contributor | CC0 1.0 |

Processing applied with ffmpeg (`-ac 1 -ar 44100 -c:a pcm_s16le`):

- silence trimmed from both ends (`silenceremove` at −50 dB);
- metadata chunks stripped (`-map_metadata -1 -fflags +bitexact`).

The files are stored at their natural level; the extension scales them down at playback
(`audio.cues.volume`, 15 % by default) so the cue stays a quiet notification.

No other edits (no pitch shift, no re-sequencing).
