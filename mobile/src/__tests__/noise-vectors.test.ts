// Replays the core-generated Noise transcripts (protocol/fixtures/noise) byte for byte.
import { sha256 } from '@noble/hashes/sha2.js';
import { fromHex, toHex, utf8Decode, utf8Encode } from '../link/bytes';
import { open, seal } from '../link/frame';
import { Initiator, NoiseError, prologue } from '../link/noise';
import type { Transport } from '../link/noise';

declare const __dirname: string;
declare const Buffer: { from: (s: string, enc: 'base64') => Uint8Array };
const fs = require('fs') as { readFileSync: (path: string, encoding: 'utf8') => string };
const DIR = `${__dirname}/../../../protocol/fixtures/noise`;

type Msg = { plaintext: string; wire_hex: string };
type Vector = {
  suite: string;
  path: string;
  request_frame: string;
  reply_frame: string;
  prologue_hex: string;
  client_static_secret_hex: string;
  client_static_public_hex: string;
  client_ephemeral_secret_hex: string;
  host_static_public_hex: string;
  psk_hex?: string;
  msg1_hex: string;
  msg2_hex: string;
  handshake_hash_hex: string;
  transport: {
    c2h_hello: Msg;
    c2h_ping: Msg;
    h2c_welcome: Msg;
    h2c_big: { wire_base64: string; wire_length: number };
    c2h_big: { wire_length: number; wire_sha256_hex: string };
    big_plaintext: { length: number; sha256_hex: string };
  };
};

const load = (name: string) => JSON.parse(fs.readFileSync(`${DIR}/${name}.json`, 'utf8')) as Vector;
const bigPlain = () => Uint8Array.from({ length: 65520 }, (_, i) => 0x61 + (i % 26));

function start(v: Vector, pskHex: string | undefined = v.psk_hex) {
  const opts = {
    prologue: prologue(v.path, v.request_frame, v.reply_frame),
    s: { secret: fromHex(v.client_static_secret_hex), public: fromHex(v.client_static_public_hex) },
    rs: fromHex(v.host_static_public_hex),
    random: () => fromHex(v.client_ephemeral_secret_hex),
  };
  return pskHex === undefined
    ? Initiator.link(opts)
    : Initiator.pair({ ...opts, psk: fromHex(pskHex) });
}

describe.each([['ikpsk2'], ['kk']])('%s vector', (name) => {
  const v = load(name);

  it('builds the prologue bytes', () => {
    expect(toHex(prologue(v.path, v.request_frame, v.reply_frame))).toBe(v.prologue_hex);
  });

  it('writes message 1 equal', () => {
    expect(toHex(start(v).writeMessage1())).toBe(v.msg1_hex);
  });

  it('reads message 2 and agrees on the handshake hash', () => {
    const hs = start(v);
    hs.writeMessage1();
    expect(hs.readMessage2(fromHex(v.msg2_hex)).length).toBe(0);
    expect(toHex(hs.split().handshakeHash)).toBe(v.handshake_hash_hex);
  });

  describe('transport', () => {
    let t: Transport;
    beforeEach(() => {
      const hs = start(v);
      hs.writeMessage1();
      hs.readMessage2(fromHex(v.msg2_hex));
      t = hs.split();
    });

    it('seals hello and ping equal', () => {
      const { c2h_hello, c2h_ping } = v.transport;
      expect(toHex(seal(t.send, utf8Encode(c2h_hello.plaintext)))).toBe(c2h_hello.wire_hex);
      expect(toHex(seal(t.send, utf8Encode(c2h_ping.plaintext)))).toBe(c2h_ping.wire_hex);
    });

    it('opens welcome and the two-chunk frame', () => {
      const { h2c_welcome, h2c_big, big_plaintext } = v.transport;
      expect(utf8Decode(open(t.recv, fromHex(h2c_welcome.wire_hex)))).toBe(h2c_welcome.plaintext);
      const wire = Uint8Array.from(Buffer.from(h2c_big.wire_base64, 'base64'));
      expect(wire.length).toBe(h2c_big.wire_length);
      const plain = open(t.recv, wire);
      expect(plain.length).toBe(big_plaintext.length);
      expect(toHex(plain)).toBe(toHex(bigPlain()));
      expect(toHex(sha256(plain))).toBe(big_plaintext.sha256_hex);
    });

    it('seals the 65520-byte frame to the recorded hash', () => {
      const { c2h_hello, c2h_ping, c2h_big } = v.transport;
      seal(t.send, utf8Encode(c2h_hello.plaintext));
      seal(t.send, utf8Encode(c2h_ping.plaintext));
      const wire = seal(t.send, bigPlain());
      expect(wire.length).toBe(c2h_big.wire_length);
      expect(toHex(sha256(wire))).toBe(c2h_big.wire_sha256_hex);
    });
  });

  it('fails when a bit of message 2 is flipped', () => {
    const msg2 = fromHex(v.msg2_hex);
    msg2[msg2.length - 1] ^= 1;
    const hs = start(v);
    hs.writeMessage1();
    expect(() => hs.readMessage2(msg2)).toThrow(NoiseError);
  });

  it('fails when a bit of the ephemeral key in message 2 is flipped', () => {
    const msg2 = fromHex(v.msg2_hex);
    msg2[3] ^= 1;
    const hs = start(v);
    hs.writeMessage1();
    expect(() => hs.readMessage2(msg2)).toThrow(NoiseError);
  });
});

describe('psk and pattern rules', () => {
  it('a wrong psk fails at message 2', () => {
    const v = load('ikpsk2');
    const hs = start(v, '00'.repeat(32));
    hs.writeMessage1();
    expect(() => hs.readMessage2(fromHex(v.msg2_hex))).toThrow(NoiseError);
  });

  it('IKpsk2 refuses a missing psk', () => {
    const v = load('ikpsk2');
    expect(() =>
      Initiator.pair({
        prologue: new Uint8Array(0),
        s: { secret: fromHex(v.client_static_secret_hex), public: fromHex(v.client_static_public_hex) },
        rs: fromHex(v.host_static_public_hex),
        random: () => new Uint8Array(32),
      }),
    ).toThrow(NoiseError);
  });

  it('split before message 2 is refused', () => {
    const hs = start(load('kk'));
    hs.writeMessage1();
    expect(() => hs.split()).toThrow(NoiseError);
  });
});
