// The only randomness the app wires into the link: native bytes from expo-crypto.
import { getRandomBytes } from 'expo-crypto';
import type { Random } from './keys';

export const random: Random = (length) => getRandomBytes(length);
