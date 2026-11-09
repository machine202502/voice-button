// Checks the PCM16 encoding the demo sends to /api/record/audio.
//
//   node api-demo/selftest.mjs

import assert from 'node:assert/strict';
import { encodePcm16 } from './pcm.js';

// A single frame: 0.5, -1, +1, 2 (clipped) and -2 (clipped).
const frames = [new Float32Array([0.5, -1, 1, 2, -2])];
const pcm = encodePcm16(frames);

assert.equal(pcm.length, 5, 'one i16 per sample');
assert.equal(pcm[0], 16383, 'positive samples scale by 32767 (truncated)');
assert.equal(pcm[1], -0x8000, '-1 is the i16 minimum');
assert.equal(pcm[2], 0x7fff, '+1 is the i16 maximum');
assert.equal(pcm[3], 0x7fff, 'above +1 clips');
assert.equal(pcm[4], -0x8000, 'below -1 clips');

const bytes = new Uint8Array(pcm.buffer);
assert.equal(bytes.length, 10, 'two bytes per sample on the wire');
// Little-endian: the low byte of 0x3fff comes first.
assert.deepEqual([...bytes.slice(0, 2)], [0xff, 0x3f]);
assert.deepEqual([...bytes.slice(2, 4)], [0x00, 0x80]);

const joined = encodePcm16([new Float32Array(160), new Float32Array(240)]);
assert.equal(joined.length, 400, 'frames are concatenated');

console.log('api-demo: PCM16 encoding OK');
