import { describe, expect, it } from 'vitest';

import { resampleBlock } from './resample';

/** A quiet ramp, so a taken sample can be told from an interpolated one. */
function ramp(length: number): Float32Array {
  return Float32Array.from({ length }, (_, index) => index / 10);
}

describe('resampling the phone microphone', () => {
  it('takes every third sample of 48 kHz audio', () => {
    const { samples, position } = resampleBlock(ramp(12), 3, 0);
    expect(samples).toHaveLength(4);
    for (const [index, expected] of [0, 0.3, 0.6, 0.9].entries()) {
      expect(samples[index]).toBeCloseTo(expected, 6);
    }
    expect(position).toBe(0);
  });

  it('keeps 44.1 kHz audio instead of turning it into silence', () => {
    const input = new Float32Array(4410).fill(0.5);
    const { samples } = resampleBlock(input, 44100 / 16000, 0);
    // The old code indexed `input[2.75625]`, got `undefined` for every sample
    // after the first, and the take recorded near-silence. A constant stays one.
    expect(samples.length).toBe(1600);
    for (const sample of samples) expect(sample).toBeCloseTo(0.5, 6);
  });

  it('carries the fractional position into the next block', () => {
    const one = resampleBlock(ramp(4410), 44100 / 16000, 0);
    const two = resampleBlock(ramp(4410), 44100 / 16000, one.position);
    expect(one.samples.length).toBe(1600);
    expect(two.samples.length).toBe(1600);
    expect(two.position).toBe(0);
  });

  it('never produces NaN for an empty or silent block', () => {
    expect(resampleBlock(new Float32Array(0), 44100 / 16000, 0)).toEqual({
      samples: [],
      position: 0,
    });
    const silence = resampleBlock(new Float32Array(1024), 44100 / 16000, 0);
    expect(silence.samples.length).toBeGreaterThan(0);
    expect(silence.samples.every((sample) => sample === 0)).toBe(true);
  });
});
