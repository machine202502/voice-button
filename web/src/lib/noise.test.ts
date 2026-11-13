import { describe, expect, it } from 'vitest';

import { bandBins, NoiseCleaner, scaleDb } from './noise';

const SAMPLE_RATE = 48_000;
const FFT = 2048;
const BINS = FFT / 2;

/** A spectrum in dBFS: `noise` everywhere, `voice` on the given bins. */
function spectrum(noiseDb: number, voiceDb?: { from: number; to: number; level: number }): Float32Array {
  const bins = new Float32Array(BINS).fill(noiseDb);
  if (voiceDb) {
    for (let bin = voiceDb.from; bin <= voiceDb.to; bin += 1) bins[bin] = voiceDb.level;
  }
  return bins;
}

function dbFromLevel(level: number): number {
  return 20 * Math.log10(Math.max(1e-9, level));
}

describe('the band', () => {
  it('covers the speech range and nothing else', () => {
    const { low, high } = bandBins(SAMPLE_RATE, FFT);
    const toHz = (bin: number): number => (bin / BINS) * (SAMPLE_RATE / 2);
    // Bins are ~23 Hz wide, so the edge lands within one bin of the requested one.
    const width = SAMPLE_RATE / 2 / BINS;
    expect(Math.abs(toHz(low) - 120)).toBeLessThanOrEqual(width);
    expect(Math.abs(toHz(high) - 4000)).toBeLessThanOrEqual(width);
  });

  it('never returns an empty or inverted range', () => {
    const { low, high } = bandBins(SAMPLE_RATE, FFT, { low: 5000, high: 6000 });
    expect(high).toBeGreaterThanOrEqual(low);
  });
});

describe('the level scale', () => {
  it('is silent at −60 dBFS and full at −6 dBFS', () => {
    expect(scaleDb(-60)).toBe(0);
    expect(scaleDb(-100)).toBe(0);
    expect(scaleDb(-6)).toBe(1);
    expect(scaleDb(0)).toBe(1);
  });

  it('half way is halfway between', () => {
    expect(scaleDb(-33)).toBeCloseTo(0.5, 2);
  });
});

describe('the noise cleaner', () => {
  it('closes on a steady hiss and opens on a voice', () => {
    const cleaner = new NoiseCleaner(SAMPLE_RATE, FFT);
    const hissDb = dbFromLevel(0.01);
    const hiss = spectrum(hissDb);

    // Two seconds of room: the gate must end up shut.
    for (let frame = 0; frame < 120; frame += 1) {
      cleaner.measure(hiss, hissDb, 1 / 60);
    }
    const silent = cleaner.measure(hiss, hissDb, 1 / 60);
    expect(silent).toBeLessThan(0.05);

    // A voice 20 dB over the room, in the middle of the band.
    const voiceDb = hissDb + 20;
    const voice = spectrum(hissDb, { from: 40, to: 90, level: voiceDb });
    let loud = 0;
    for (let frame = 0; frame < 12; frame += 1) {
      loud = cleaner.measure(voice, voiceDb, 1 / 60);
    }
    expect(loud).toBeGreaterThan(0.5);
  });

  it('ignores hiss outside the band', () => {
    const cleaner = new NoiseCleaner(SAMPLE_RATE, FFT);
    const levelDb = dbFromLevel(0.3);
    // Everything below 120 Hz: rumble of a fridge, hum of a lamp.
    const rumble = spectrum(-100, { from: 1, to: 3, level: dbFromLevel(0.3) });
    let value = 1;
    for (let frame = 0; frame < 120; frame += 1) {
      value = cleaner.measure(rumble, levelDb, 1 / 60);
    }
    expect(value).toBeLessThan(0.05);
  });

  it('learns again after forget', () => {
    const cleaner = new NoiseCleaner(SAMPLE_RATE, FFT);
    const hissDb = dbFromLevel(0.01);
    const hiss = spectrum(hissDb);
    for (let frame = 0; frame < 120; frame += 1) cleaner.measure(hiss, hissDb, 1 / 60);
    expect(cleaner.measure(hiss, hissDb, 1 / 60)).toBeLessThan(0.05);

    cleaner.forget();
    // Straight after forgetting, the room is unknown, so the same hiss reads as
    // signal: that is what "learn again" means.
    expect(cleaner.measure(hiss, hissDb, 1 / 60)).toBeGreaterThan(0.05);
  });
});
