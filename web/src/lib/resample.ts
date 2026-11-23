// Streaming mono resampling for the phone's recorder.
//
// The browser hands audio at the AudioContext's rate (48 kHz, but 44.1 kHz on
// many devices), and the computer stores 16 kHz PCM. The ratio is therefore
// often fractional (44100 / 16000 = 2.75625). Walking the input with an integer
// step on such a ratio makes every position but the first fractional;
// `input[fractional]` is `undefined`, and the recorded take becomes silence.
//
// One block cannot be resampled on its own: an output sample usually sits
// between two input samples, and the last one may straddle two blocks. The
// caller keeps `position` and hands it back with the next block.

/**
 * Resample one block: `step` input samples per output sample, `position` is
 * where the next output sample starts in this block.
 */
export function resampleBlock(
  input: Float32Array,
  step: number,
  position: number,
): { samples: number[]; position: number } {
  const samples: number[] = [];
  let at = position;
  // A ratio like 44100 / 16000 is not exact in binary: without the epsilon the
  // last step lands at 4409.999999… and the block yields one sample too many,
  // which drifts the recorded rate a little on every block.
  const end = input.length - 1e-6;
  while (at < end) {
    const index = Math.floor(at);
    const fraction = at - index;
    const first = input[index] ?? 0;
    // The last output of a block often lands between this block's final sample
    // and the next block's first one; holding the last value for that single
    // sample keeps a click out of the seam.
    const second = input[index + 1] ?? first;
    samples.push(first + (second - first) * fraction);
    at += step;
  }
  // The next block starts where this one stopped. Round the floating residue
  // off: a position of -1e-10 would index `input[-1]` and add one sample.
  const next = at - input.length;
  return { samples, position: Math.abs(next) < 1e-6 ? 0 : next };
}
