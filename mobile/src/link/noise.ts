// Initiator-only Noise for exactly two patterns, hand-written over @noble (G1):
//   Noise_IKpsk2_25519_ChaChaPoly_SHA256  pre <- s;  -> e, es, s, ss;  <- e, ee, se, psk
//   Noise_KK_25519_ChaChaPoly_SHA256      pre -> s, <- s;  -> e, es, ss;  <- e, ee, se
// No responder, no rekey, no pattern parser. The handshake payloads are empty.
// Randomness is never drawn here: the caller passes the random function in.
import { chacha20poly1305 } from '@noble/ciphers/chacha.js';
import { x25519 } from '@noble/curves/ed25519.js';
import { hmac } from '@noble/hashes/hmac.js';
import { sha256 } from '@noble/hashes/sha2.js';
import { concat, u32be, utf8Encode } from './bytes';
import type { KeyPair, Random } from './keys';
import { publicKey } from './keys';

export const SUITE_PAIR = 'Noise_IKpsk2_25519_ChaChaPoly_SHA256';
export const SUITE_LINK = 'Noise_KK_25519_ChaChaPoly_SHA256';
export const MAX_NOISE_MESSAGE = 65535;
export const TAG_BYTES = 16;

const PROLOGUE_TAG = 'orchestra-link v1';
const EMPTY = new Uint8Array(0);

export class NoiseError extends Error {}

// The exact strings sent on the wire, never re-serialised.
export function prologue(path: string, requestText: string, replyText: string): Uint8Array {
  const part = (s: string) => {
    const b = utf8Encode(s);
    return concat(u32be(b.length), b);
  };
  return concat(utf8Encode(PROLOGUE_TAG), part(path), part(requestText), part(replyText));
}

export class CipherState {
  private n = 0n;
  constructor(private readonly k: Uint8Array | null) {}

  private nonce(): Uint8Array {
    if (this.n >= 0xffffffffffffffffn) throw new NoiseError('nonce exhausted');
    const out = new Uint8Array(12);
    new DataView(out.buffer).setBigUint64(4, this.n, true);
    return out;
  }

  encrypt(plaintext: Uint8Array, ad: Uint8Array = EMPTY): Uint8Array {
    if (!this.k) return plaintext;
    if (plaintext.length + TAG_BYTES > MAX_NOISE_MESSAGE) throw new NoiseError('message too long');
    const out = chacha20poly1305(this.k, this.nonce(), ad).encrypt(plaintext);
    this.n += 1n;
    return out;
  }

  decrypt(ciphertext: Uint8Array, ad: Uint8Array = EMPTY): Uint8Array {
    if (!this.k) return ciphertext;
    if (ciphertext.length < TAG_BYTES || ciphertext.length > MAX_NOISE_MESSAGE) {
      throw new NoiseError('bad ciphertext length');
    }
    let out: Uint8Array;
    try {
      out = chacha20poly1305(this.k, this.nonce(), ad).decrypt(ciphertext);
    } catch {
      throw new NoiseError('decrypt failed');
    }
    this.n += 1n;
    return out;
  }
}

// HKDF with HMAC-SHA256 as Noise defines it, returning two or three outputs.
function hkdf(ck: Uint8Array, ikm: Uint8Array, outputs: 2 | 3): Uint8Array[] {
  const temp = hmac(sha256, ck, ikm);
  const o1 = hmac(sha256, temp, Uint8Array.of(1));
  const o2 = hmac(sha256, temp, concat(o1, Uint8Array.of(2)));
  if (outputs === 2) return [o1, o2];
  return [o1, o2, hmac(sha256, temp, concat(o2, Uint8Array.of(3)))];
}

export class SymmetricState {
  h: Uint8Array;
  ck: Uint8Array;
  private cs = new CipherState(null);

  constructor(name: string) {
    const bytes = utf8Encode(name);
    // The name is longer than 32 bytes for both suites, so it is hashed.
    this.h = bytes.length <= 32 ? concat(bytes, new Uint8Array(32 - bytes.length)) : sha256(bytes);
    this.ck = this.h;
  }

  mixHash(data: Uint8Array): void {
    this.h = sha256(concat(this.h, data));
  }

  mixKey(ikm: Uint8Array): void {
    const [ck, k] = hkdf(this.ck, ikm, 2);
    this.ck = ck;
    this.cs = new CipherState(k);
  }

  mixKeyAndHash(ikm: Uint8Array): void {
    const [ck, tempH, tempK] = hkdf(this.ck, ikm, 3);
    this.ck = ck;
    this.mixHash(tempH);
    this.cs = new CipherState(tempK);
  }

  encryptAndHash(plaintext: Uint8Array): Uint8Array {
    const ct = this.cs.encrypt(plaintext, this.h);
    this.mixHash(ct);
    return ct;
  }

  decryptAndHash(ciphertext: Uint8Array): Uint8Array {
    const pt = this.cs.decrypt(ciphertext, this.h);
    this.mixHash(ciphertext);
    return pt;
  }

  split(): [CipherState, CipherState] {
    const [k1, k2] = hkdf(this.ck, EMPTY, 2);
    return [new CipherState(k1), new CipherState(k2)];
  }
}

export type Transport = { send: CipherState; recv: CipherState; handshakeHash: Uint8Array };

export type InitiatorOptions = {
  prologue: Uint8Array;
  // The phone's static key pair and the host's static public key.
  s: KeyPair;
  rs: Uint8Array;
  // Required for IKpsk2 and absent for KK.
  psk?: Uint8Array;
  random: Random;
};

export class Initiator {
  private readonly ss: SymmetricState;
  private e: KeyPair | null = null;
  private done = false;

  private constructor(
    private readonly pair: boolean,
    private readonly o: InitiatorOptions,
  ) {
    if (typeof o.random !== 'function') throw new NoiseError('random is required');
    if (o.s.secret.length !== 32 || o.rs.length !== 32) throw new NoiseError('bad key length');
    if (pair && o.psk?.length !== 32) throw new NoiseError('IKpsk2 needs a 32-byte psk');
    this.ss = new SymmetricState(pair ? SUITE_PAIR : SUITE_LINK);
    this.ss.mixHash(o.prologue);
    if (!pair) this.ss.mixHash(o.s.public);
    this.ss.mixHash(o.rs);
  }

  static pair(o: InitiatorOptions): Initiator {
    return new Initiator(true, o);
  }

  static link(o: InitiatorOptions): Initiator {
    return new Initiator(false, { ...o, psk: undefined });
  }

  // Message 1: e, es, then s, ss on IKpsk2 or just ss on KK, then an empty payload.
  writeMessage1(): Uint8Array {
    if (this.e) throw new NoiseError('message 1 already written');
    const secret = this.o.random(32);
    if (secret.length !== 32) throw new NoiseError('random returned the wrong length');
    const e = { secret, public: publicKey(secret) };
    this.e = e;
    const ss = this.ss;
    ss.mixHash(e.public);
    // A psk handshake also mixes the ephemeral key into the chaining key.
    if (this.pair) ss.mixKey(e.public);
    ss.mixKey(x25519.getSharedSecret(e.secret, this.o.rs));
    const parts: Uint8Array[] = [e.public];
    if (this.pair) parts.push(ss.encryptAndHash(this.o.s.public));
    ss.mixKey(x25519.getSharedSecret(this.o.s.secret, this.o.rs));
    parts.push(ss.encryptAndHash(EMPTY));
    return concat(...parts);
  }

  // Message 2: e, ee, se, then psk on IKpsk2, then an empty payload. Returns the payload.
  readMessage2(message: Uint8Array): Uint8Array {
    if (!this.e || this.done) throw new NoiseError('message 1 not written');
    if (message.length < 32 + TAG_BYTES || message.length > MAX_NOISE_MESSAGE) {
      throw new NoiseError('bad message 2 length');
    }
    const ss = this.ss;
    const re = message.slice(0, 32);
    ss.mixHash(re);
    if (this.pair) ss.mixKey(re);
    ss.mixKey(x25519.getSharedSecret(this.e.secret, re));
    ss.mixKey(x25519.getSharedSecret(this.o.s.secret, re));
    if (this.pair) ss.mixKeyAndHash(this.o.psk!);
    const payload = ss.decryptAndHash(message.slice(32));
    this.done = true;
    return payload;
  }

  // Valid once message 2 has been read. The initiator sends with the first key.
  split(): Transport {
    if (!this.done) throw new NoiseError('handshake not finished');
    const [send, recv] = this.ss.split();
    return { send, recv, handshakeHash: this.ss.h };
  }
}
