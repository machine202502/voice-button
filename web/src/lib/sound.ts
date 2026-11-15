// The two cues that mark the edges of a take.
//
// They are the desktop app's own `assets/start.wav` and `assets/end.wav`, copied
// into `src/sounds` and imported through Vite, so the build hashes them and
// `scripts/web.ps1` puts them into the service worker's precache: the cues work
// with no network, like the rest of the page.
//
// The switch lives in Settings and belongs to the phone (`localStorage`), the
// way the theme does. By default the cues play.

import startUrl from '../sounds/start.wav?url';
import endUrl from '../sounds/end.wav?url';

const KEY = 'vb_sounds';

/** How loud a cue is: it is short, and the phone is next to the person. */
const VOLUME = 0.6;

let enabled = readSounds();
let start: HTMLAudioElement | null = null;
let end: HTMLAudioElement | null = null;

/** The saved switch; on when nothing was saved or storage is denied. */
export function readSounds(): boolean {
  try {
    return localStorage.getItem(KEY) !== 'off';
  } catch {
    return true;
  }
}

/** Remember the switch for the next visit. */
export function saveSounds(value: boolean): void {
  enabled = value;
  try {
    localStorage.setItem(KEY, value ? 'on' : 'off');
  } catch {
    // Private mode: the choice lasts this visit.
  }
}

/** The element for one cue, created once and reused. */
function cue(which: 'start' | 'end'): HTMLAudioElement {
  if (which === 'start') {
    start ??= new Audio(startUrl);
    return start;
  }
  end ??= new Audio(endUrl);
  return end;
}

function play(which: 'start' | 'end'): void {
  if (!enabled) return;
  try {
    const sound = cue(which);
    sound.volume = VOLUME;
    sound.currentTime = 0;
    const started = sound.play();
    if (started) {
      // A browser that refuses before the first gesture: the cue is a nicety,
      // never a reason to fail the take.
      void started.catch(() => undefined);
    }
  } catch {
    // No audio in this browser; the recording does not care.
  }
}

/** The take started. */
export function playStartCue(): void {
  play('start');
}

/** The take stopped. */
export function playEndCue(): void {
  play('end');
}

/**
 * Let both cues be played later.
 *
 * A phone refuses sound that no gesture asked for, and the start cue lands after
 * the microphone permission prompt, where the gesture is already gone. A silent
 * play on the first touch unlocks the elements for the rest of the visit.
 */
export function unlockSounds(): void {
  for (const which of ['start', 'end'] as const) {
    try {
      const sound = cue(which);
      sound.muted = true;
      const started = sound.play();
      if (started) {
        void started
          .then(() => {
            sound.pause();
            sound.currentTime = 0;
            sound.muted = false;
          })
          .catch(() => {
            sound.muted = false;
          });
      }
    } catch {
      // Nothing to unlock.
    }
  }
}
