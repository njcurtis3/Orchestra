// Runs the shared protocol fixtures under jest-expo, which proves the mobile toolchain
// (Babel, jest, tsc) compiles protocol/ as plain source from ../../../protocol. The desktop
// vitest run is the canonical one; this guards the second consumer.
//
// The fixture files are read with fs rather than through protocol/fixtures/index.ts: that
// file default-imports JSON, which Babel compiles to a @babel/runtime helper, and protocol/
// has no node_modules to resolve it from.
import {
  ERROR_CODES,
  EVENT_NAMES,
  MAX_ID_BYTES,
  MAX_MESSAGE_BYTES,
  MAX_TEXT_BYTES,
  METHODS,
  METHOD_SCOPE,
  PROTOCOL_VERSION,
  ProtocolError,
  SCOPES,
  SUPPORTED_VERSIONS,
  decodeClientMessage,
  decodeHandshakeReply,
  decodeHandshakeRequest,
  decodeHostMessage,
  encodeClientMessage,
  encodeHandshakeReply,
  encodeHandshakeRequest,
  encodeHostMessage,
} from '../../../protocol';
import type { FixtureCase } from '../../../protocol/fixtures';

// tsconfig.json lists only the jest types, so the node modules are typed here, narrowly.
declare const __dirname: string;
const fs = require('fs') as {
  readFileSync: (path: string, encoding: 'utf8') => string;
  readdirSync: (path: string) => string[];
};
const DIR = `${__dirname}/../../../protocol/fixtures`;
const readJson = (name: string): unknown => JSON.parse(fs.readFileSync(`${DIR}/${name}`, 'utf8'));

const files = fs
  .readdirSync(DIR)
  .filter((n) => n.endsWith('.json') && n !== 'constants.json')
  .sort();
const cases = files.map((n) => readJson(n) as FixtureCase);

type Codec = { decode: (text: string) => unknown; encode: (v: never) => string };
const CODECS: Record<FixtureCase['dir'], Codec> = {
  c2h: { decode: decodeClientMessage, encode: encodeClientMessage },
  h2c: { decode: decodeHostMessage, encode: encodeHostMessage },
  hs_c2h: { decode: decodeHandshakeRequest, encode: encodeHandshakeRequest },
  hs_h2c: { decode: decodeHandshakeReply, encode: encodeHandshakeReply },
};

describe('protocol fixtures', () => {
  it('finds the whole fixture set', () => {
    expect(cases.length).toBeGreaterThanOrEqual(80);
    expect(cases.map((c, i) => `${c.name}.json` === files[i])).not.toContain(false);
  });

  it.each(cases.map((c) => [c.name, c] as const))('%s', (_name, c) => {
    const text = c.raw ?? JSON.stringify(c.wire);
    const { decode, encode } = CODECS[c.dir];
    if (c.expect === 'ok') {
      const decoded = decode(text);
      expect(JSON.parse(encode(decoded as never))).toEqual(c.decoded_wire ?? c.wire);
    } else {
      let err: unknown;
      try {
        decode(text);
      } catch (e) {
        err = e;
      }
      expect(err).toBeInstanceOf(ProtocolError);
      expect((err as ProtocolError).code).toBe(c.expect.error);
    }
  });

  it('constants match fixtures/constants.json', () => {
    expect(readJson('constants.json')).toEqual({
      v: PROTOCOL_VERSION,
      supported: [...SUPPORTED_VERSIONS],
      max_text_bytes: MAX_TEXT_BYTES,
      max_message_bytes: MAX_MESSAGE_BYTES,
      max_id_bytes: MAX_ID_BYTES,
      methods: [...METHODS],
      method_scope: METHOD_SCOPE,
      event_names: [...EVENT_NAMES],
      scopes: [...SCOPES],
      error_codes: [...ERROR_CODES],
    });
  });
});
