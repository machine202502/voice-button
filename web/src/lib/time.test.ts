import { describe, expect, it } from 'vitest';
import { formatDuration } from './time';

describe('formatDuration', () => {
  it('formats zero', () => {
    expect(formatDuration(0)).toBe('00:00');
  });

  it('pads seconds and minutes', () => {
    expect(formatDuration(5)).toBe('00:05');
    expect(formatDuration(65)).toBe('01:05');
  });

  it('keeps counting minutes past an hour', () => {
    expect(formatDuration(3600)).toBe('60:00');
  });

  it('clamps negative values', () => {
    expect(formatDuration(-4)).toBe('00:00');
  });
});
