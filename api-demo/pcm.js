/**
 * Pack mono f32 frames into little-endian PCM16, the format
 * `POST /api/record/audio` expects. Shared with selftest.mjs.
 */
export function encodePcm16(frames) {
  let total = 0;
  for (const frame of frames) total += frame.length;
  const pcm = new Int16Array(total);
  let index = 0;
  for (const frame of frames) {
    for (let i = 0; i < frame.length; i++) {
      const sample = Math.max(-1, Math.min(1, frame[i]));
      pcm[index++] = sample < 0 ? sample * 0x8000 : sample * 0x7fff;
    }
  }
  return pcm;
}
