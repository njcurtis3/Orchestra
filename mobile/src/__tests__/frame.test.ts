import { MAX_MESSAGE_BYTES } from '../../../protocol/constants';
import { CipherState } from '../link/noise';
import { FrameError, MAX_CHUNK, open, seal } from '../link/frame';

const key = () => Uint8Array.from({ length: 32 }, (_, i) => i + 1);
const pair = () => [new CipherState(key()), new CipherState(key())] as const;
const bytes = (n: number) => Uint8Array.from({ length: n }, (_, i) => i % 251);

function chunksIn(wire: Uint8Array): number {
  let n = 0;
  for (let at = 0; at < wire.length; n++) at += 2 + ((wire[at] << 8) | wire[at + 1]);
  return n;
}

function codeOf(fn: () => unknown): string | undefined {
  try {
    fn();
  } catch (e) {
    return e instanceof FrameError ? e.code : 'other';
  }
  return undefined;
}

describe('frame', () => {
  it('splits at 65519 bytes', () => {
    expect(MAX_CHUNK).toBe(65519);
    expect(chunksIn(seal(pair()[0], bytes(65519)))).toBe(1);
    expect(chunksIn(seal(pair()[0], bytes(65520)))).toBe(2);
  });

  it('round trips empty, small and multi-chunk messages', () => {
    for (const n of [0, 1, 65519, 65520, 200000]) {
      const [a, b] = pair();
      expect(open(b, seal(a, bytes(n)))).toEqual(bytes(n));
    }
  });

  it('seals an empty message as one empty chunk', () => {
    const wire = seal(pair()[0], new Uint8Array(0));
    expect(wire.length).toBe(2 + 16);
    expect(chunksIn(wire)).toBe(1);
  });

  it('a flipped chunk byte fails to open', () => {
    const [a, b] = pair();
    const wire = seal(a, bytes(100));
    wire[10] ^= 1;
    expect(codeOf(() => open(b, wire))).toBe('Decrypt');
  });

  it('refuses a frame over the cap as TooLarge before decrypting', () => {
    const [a, b] = pair();
    const wire = seal(a, bytes(65520));
    // Corrupt the second chunk: TooLarge must win, so it was never decrypted.
    wire[wire.length - 1] ^= 1;
    expect(codeOf(() => open(b, wire, 65519))).toBe('TooLarge');
  });

  it('a message at the cap opens and one byte past it does not', () => {
    const [a, b] = pair();
    expect(open(b, seal(a, bytes(MAX_MESSAGE_BYTES))).length).toBe(MAX_MESSAGE_BYTES);
    const [c, d] = pair();
    expect(codeOf(() => open(d, seal(c, bytes(MAX_MESSAGE_BYTES + 1))))).toBe('TooLarge');
  });

  it('refuses malformed wire bytes', () => {
    const [, b] = pair();
    expect(codeOf(() => open(b, new Uint8Array(0)))).toBe('Malformed');
    expect(codeOf(() => open(b, Uint8Array.of(0)))).toBe('Malformed');
    expect(codeOf(() => open(b, Uint8Array.of(0, 15, ...new Uint8Array(15))))).toBe('Malformed');
    expect(codeOf(() => open(b, Uint8Array.of(0, 20, 1, 2, 3)))).toBe('Malformed');
  });
});
