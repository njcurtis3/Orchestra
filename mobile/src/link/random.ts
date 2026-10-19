// The only randomness the app wires into the link: native bytes from expo-crypto.
// getRandomValues goes straight to the native module. getRandomBytes would fall back to
// Math.random in a dev bundle on bridgeless React Native (Expo Go), which is never a key source.
import { getRandomValues } from 'expo-crypto';
import type { Random } from './keys';

export const random: Random = (length) => {
  if (typeof getRandomValues !== 'function') throw new Error('secure random is unavailable');
  const out = getRandomValues(new Uint8Array(length));
  if (!(out instanceof Uint8Array) || out.length !== length) {
    throw new Error('secure random returned the wrong shape');
  }
  return out;
};
