// Turning a microphone signal into "how much the person is saying".
//
// A phone's microphone always hears the room: a fridge hissing, a fan, a desk
// lamp humming. Feeding that straight into the animation makes the rings ripple
// at nothing at all. Three cheap layers fix it, in this order:
//
// 1. a band. Hum and rumble live below ~120 Hz, hiss above ~4 kHz, and speech
//    carries almost nothing there;
// 2. a per-frequency noise floor, learned as the signal plays. It follows the
//    spectrum down quickly and back up slowly, so a steady hiss settles into it
//    and gets subtracted, while a voice — which keeps changing frequencies —
//    never stays long enough to become the floor;
// 3. a gate with hysteresis on what is left, so the level cannot flicker around
//    the threshold.
//
// The numbers below are the ones chosen by ear on a real room: threshold 10 dB,
// band 120–4000 Hz. They are plain data, so a settings screen can move them later.

/** Below this, a tone is the room's, not the person's. */
export const BAND_LOW_HZ = 120;
/** Above this there is hiss and almost no speech. */
export const BAND_HIGH_HZ = 4000;
/** How far a signal must rise above the learned floor to be speech. */
export const GATE_DB = 10;
/** Opening takes +3 dB, closing −3 dB: the difference that stops the flicker. */
const HYSTERESIS_DB = 3;
/** Quietend of the scale, dBFS. */
const FLOOR_DB = -60;
/** Loudest a phone microphone gets in practice, dBFS. */
const CEIL_DB = -6;
/** The learned floor cannot go below this, so digital silence teaches nothing. */
const LEARN_LIMIT_DB = -120;
/** Per second: down fast, up slow. A steady room settles in about a second. */
const LEARN_DOWN = 2.2;
const LEARN_UP = 1.2;
/** Subtraction headroom: a little more than the measured floor. */
const SUBTRACT = 1.6;

export interface Range {
  low: number;
  high: number;
}

/** The bin numbers the band covers, clamped to the spectrum. */
export function bandBins(sampleRate: number, fftSize: number, range: Range = band()): Range {
  const bins = fftSize / 2;
  const nyquist = sampleRate / 2;
  const low = Math.max(1, Math.floor((range.low / nyquist) * bins));
  const high = Math.min(bins - 1, Math.ceil((range.high / nyquist) * bins));
  return { low, high: Math.max(low, high) };
}

export function band(): Range {
  return { low: BAND_LOW_HZ, high: BAND_HIGH_HZ };
}

/** 0..1 from a level in dBFS. */
export function scaleDb(db: number): number {
  if (!Number.isFinite(db)) return 0;
  return Math.min(1, Math.max(0, (db - FLOOR_DB) / (CEIL_DB - FLOOR_DB)));
}

/**
 * Learns the room and answers how much of the signal is the person.
 *
 * One instance per recording. `learn` must be called for every analysed frame,
 * even while the gate is closed — that is exactly when the room is measured.
 */
export class NoiseCleaner {
  private readonly noise: Float32Array;
  private readonly bins: Range;
  private clean = 0;

  constructor(
    private readonly sampleRate: number,
    private readonly fftSize: number,
    private readonly gateDb = GATE_DB,
    private readonly range: Range = band(),
  ) {
    // Nothing is known yet: no subtraction, and the gate open, until the room is
    // measured. Guessing "the room is loud" instead would silence speech for the
    // first seconds of every recording.
    this.noise = new Float32Array(fftSize / 2).fill(LEARN_LIMIT_DB);
    this.bins = bandBins(sampleRate, fftSize, range);
  }

  /** Forget the learned room: the next frames teach it again. */
  forget(): void {
    this.noise.fill(LEARN_LIMIT_DB);
    this.clean = 0;
  }

  /** How much of this frame is speech, 0..1. Feed it the spectrum in dBFS. */
  measure(spectrumDb: Float32Array, levelDb: number, dtSeconds: number): number {
    const { low, high } = this.bins;
    let kept = 0;
    let total = 0;
    for (let bin = low; bin <= high; bin += 1) {
      const power = Math.pow(10, (spectrumDb[bin] ?? -120) / 10);
      const floor = Math.pow(10, (this.noise[bin] ?? -120) / 10);
      kept += Math.max(0, power - floor * SUBTRACT);
      total += power;

      const observed = Number.isFinite(spectrumDb[bin])
        ? Math.max(spectrumDb[bin] ?? -120, LEARN_LIMIT_DB)
        : LEARN_LIMIT_DB;
      const rate = observed < (this.noise[bin] ?? LEARN_LIMIT_DB) ? LEARN_DOWN : LEARN_UP;
      const previous = this.noise[bin] ?? LEARN_LIMIT_DB;
      this.noise[bin] = previous + (observed - previous) * Math.min(1, rate * dtSeconds);
    }

    const share = total > 0 ? kept / total : 0;
    let value = scaleDb(levelDb) * Math.min(1, Math.sqrt(share) * 1.2);

    // Hysteresis: a gate that opens and closes at the same level makes the rings
    // shimmer at every breath.
    const open = this.clean > 0.02;
    const needed = this.gateDb + (open ? -HYSTERESIS_DB : HYSTERESIS_DB);
    const over = levelDb - this.floorDb();
    if (over < needed) value = 0;
    else value *= Math.min(1, (over - needed) / 9 + 0.35);

    this.clean = Math.min(1, Math.max(0, value));
    return this.clean;
  }

  /**
   * The room's own level, in dBFS: the loudest of the learned bins is what the
   * gate must rise above, so the quietest corner of the room cannot open it.
   */
  private floorDb(): number {
    const { low, high } = this.bins;
    let loudest = LEARN_LIMIT_DB;
    for (let bin = low; bin <= high; bin += 1) {
      const value = this.noise[bin] ?? LEARN_LIMIT_DB;
      if (value > loudest) loudest = value;
    }
    return loudest;
  }
}
