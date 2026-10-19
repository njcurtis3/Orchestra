// One application message = one WebSocket binary message holding a run of
// [u16 big-endian length][Noise ciphertext] chunks (mirrors core/src/link/frame.rs).
import { MAX_MESSAGE_BYTES } from '../../../protocol/constants';
import { concat } from './bytes';
import type { CipherState } from './noise';
import { MAX_NOISE_MESSAGE, TAG_BYTES } from './noise';

export const MAX_CHUNK = MAX_NOISE_MESSAGE - TAG_BYTES;
const LEN_BYTES = 2;

export type FrameErrorCode = 'Malformed' | 'TooLarge' | 'Decrypt';

export class FrameError extends Error {
  constructor(readonly code: FrameErrorCode) {
    super(code);
  }
}

// An empty message is one empty chunk.
export function seal(cipher: CipherState, plaintext: Uint8Array): Uint8Array {
  const parts: Uint8Array[] = [];
  const chunk = (bytes: Uint8Array) => {
    const ct = cipher.encrypt(bytes);
    parts.push(Uint8Array.of(ct.length >> 8, ct.length & 255), ct);
  };
  if (plaintext.length === 0) chunk(plaintext);
  for (let at = 0; at < plaintext.length; at += MAX_CHUNK) {
    chunk(plaintext.subarray(at, at + MAX_CHUNK));
  }
  return concat(...parts);
}

// Refuses as soon as the plaintext would pass maxPlain, before decrypting that chunk.
export function open(
  cipher: CipherState,
  wire: Uint8Array,
  maxPlain: number = MAX_MESSAGE_BYTES,
): Uint8Array {
  if (wire.length === 0) throw new FrameError('Malformed');
  const parts: Uint8Array[] = [];
  let total = 0;
  let at = 0;
  while (at < wire.length) {
    if (wire.length - at < LEN_BYTES) throw new FrameError('Malformed');
    const len = (wire[at] << 8) | wire[at + 1];
    at += LEN_BYTES;
    if (len < TAG_BYTES || len > wire.length - at) throw new FrameError('Malformed');
    if (total + (len - TAG_BYTES) > maxPlain) throw new FrameError('TooLarge');
    let pt: Uint8Array;
    try {
      pt = cipher.decrypt(wire.subarray(at, at + len));
    } catch {
      throw new FrameError('Decrypt');
    }
    parts.push(pt);
    total += pt.length;
    at += len;
  }
  return concat(...parts);
}
