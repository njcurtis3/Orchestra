// Static key helpers. The random source is always passed in.
import { x25519 } from '@noble/curves/ed25519.js';
import { sha256 } from '@noble/hashes/sha2.js';
import { toHex } from './bytes';

export type Random = (length: number) => Uint8Array;

export type KeyPair = { secret: Uint8Array; public: Uint8Array };

export function publicKey(secret: Uint8Array): Uint8Array {
  return x25519.getPublicKey(secret);
}

// First 16 bytes of SHA-256 of the public key, lowercase hex.
export function hostId(pub: Uint8Array): string {
  return toHex(sha256(pub).slice(0, 16));
}

// First 8 bytes of SHA-256, hex, in groups of four joined by a dash.
export function fingerprint(pub: Uint8Array): string {
  return toHex(sha256(pub).slice(0, 8)).match(/.{4}/g)!.join('-');
}

export function generateStaticKey(random: Random): KeyPair {
  const secret = random(32);
  if (secret.length !== 32) throw new Error('random returned the wrong length');
  return { secret, public: publicKey(secret) };
}
