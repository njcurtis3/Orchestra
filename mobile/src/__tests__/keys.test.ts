import { fromHex, toHex } from '../link/bytes';
import { fingerprint, generateStaticKey, hostId, publicKey } from '../link/keys';

declare const __dirname: string;
const fs = require('fs') as { readFileSync: (path: string, encoding: 'utf8') => string };
const DIR = `${__dirname}/../../../protocol/fixtures/noise`;
const read = (name: string) => JSON.parse(fs.readFileSync(`${DIR}/${name}.json`, 'utf8'));

describe('keys', () => {
  const offerFile = read('offer');
  const offer = JSON.parse(offerFile.json);
  const vector = read('ikpsk2');

  it('hostId of the offer host key is the offer host_id', () => {
    expect(hostId(fromHex(offer.host_key))).toBe(offer.host_id);
  });

  it('derives the public key from the secret', () => {
    expect(toHex(publicKey(fromHex(vector.client_static_secret_hex)))).toBe(
      vector.client_static_public_hex,
    );
  });

  it('fingerprints the client key as the fixture records', () => {
    expect(fingerprint(fromHex(vector.client_static_public_hex))).toBe(
      offerFile.fingerprint_of_client_key,
    );
  });

  it('generates a static key from the injected random', () => {
    const secret = fromHex(vector.client_static_secret_hex);
    const key = generateStaticKey(() => secret);
    expect(toHex(key.public)).toBe(vector.client_static_public_hex);
  });

  it('refuses a random source of the wrong length', () => {
    expect(() => generateStaticKey(() => new Uint8Array(31))).toThrow();
  });
});
