# protocol/ — the client <-> host contract

The contract between a **host**, which runs the agents, and the **clients** that watch and
steer them. Both sides are built from this definition. This file is a decision record in
prose: each section says what was chosen and what would reopen it. The wire types are code
beside it: `types.ts`, `constants.ts` and `codec.ts` define protocol v1, and `fixtures/` holds
the JSON cases both copies are tested against.

- **One protocol, versioned here.** Every message carries the protocol version.
- **A protocol change ships in both clients together.** A change that one client cannot
  speak is not merged until the other can. Desktop and mobile release on separate channels
  (installers vs. the Play Store), so the older client must fail loudly on a version it
  does not understand, never guess.
- **The TypeScript definitions here are the source of truth.** The Rust copy is
  `orchestra_core::wire` (`core/src/wire/`) and is checked against them by tests that run the
  same `fixtures/*.json`.

## Scope

A **host** is the desktop app, or `orchestrad` (H2) on a headless server. Both run
`orchestra-core`. A **client** is the phone, or the desktop app attached to a remote host
(H3). One protocol serves both. The link is **off by default** on every host.

## Reachability

**Decided: A.** A client connects directly to an address the host binds: loopback, a LAN
interface, or a private overlay the user already runs (Tailscale or WireGuard). Away from
home means the overlay. The host binds only the interfaces named in its config or UI, never
`0.0.0.0` by default. A rented bare-metal box is reached over the user's tailnet. There is
no open public port and no server of ours.

Rejected for v1: B, a public TLS endpoint with pinned keys, because it needs an open port. C, an
SSH tunnel, because the phone has no maintained SSH client on Expo.

**Reopened if** the owner will not require an overlay for away-from-home use. Then a relay
(an ops item, a server of ours) or B behind an explicit opt-in.

## Transport

**Decided: A.** A WebSocket (`ws://`) served by `orchestra-core`, carrying a Noise protocol
channel: static X25519 keys, ChaChaPoly AEAD per frame. Rust uses the `snow` crate. The
phone uses a JS Noise implementation on `@noble` primitives, checked against the Noise test
vectors. (A1 amends this: the vectors are snow transcripts in `fixtures/noise`; see "What A1
decided".)

Why: every frame is authenticated and encrypted by construction, both ends know each other's
static key, nonces give replay and reorder protection, and it needs no CA, no certificate
pinning inside React Native's OkHttp (B's weak point), and no server of ours (C). When the
desktop is the client (H3), the Rust process owns the socket and the keys, so the webview
never holds a key and CSP `connect-src` needs no new origin for the client role.

Rejected: B, WSS with a self-signed host cert pinned at pairing plus client certs (mutual
TLS). C, a LAN WebSocket plus a relay we run for away-from-home.

**Reopened if** no maintained JS Noise implementation passes the test vectors on Hermes in
P2. Then libsodium (`crypto_kx` + `secretstream`, via `react-native-libsodium`) under the
same message layer. Or if a maintained Expo module makes pinned mutual TLS straightforward,
then B.

## Pairing

**Decided: A, with confirmation on the host.** The host (desktop window, or the `orchestrad`
terminal for H2) shows a QR code carrying `{v, host_id, host name, addresses + port, host
static public key, one-time secret (256-bit as built; see "What P3 decided"), expiry}`. The handshake is Noise `IKpsk2` with
the secret as the psk; the client sends its own static key inside it. The host then shows the
client name and key fingerprint, and the user **confirms on the host** before the client is
stored.

- The pairing window opens only on user action, lasts 5 minutes, and is single use.
- Later connections use Noise `KK` (both static keys known). An unknown client key is closed
  with no information.
- Phone: the keypair is generated on the device; the private key lives only in
  `expo-secure-store`. Desktop as client: the OS keyring.
- Host private key: the OS keyring (desktop) or a `0600` file in the `orchestrad` state
  directory (H2).
- The host key is pinned at pairing. A changed host key is refused loudly. (This was an H3
  item; it is now protocol-wide.)

Rejected: B, a short typed code with a PAKE (SPAKE2) instead of a QR. C, trust on first use
with no secret.

**Reopened if** a host cannot show a QR (no terminal QR, no screen). Then add B as a second
pairing mode.

## Authorisation

**Decided:** per-client scopes, stored on the host with the client key.

| Scope | Allows |
|---|---|
| `read` | snapshot, events, `get_transcript`, `list_*`, `get_task_diff`, `storage_status` |
| `answer` | `answer` (permission, question, reply) |
| `control` | `start_session`, `stop_session`, `create_task`, `cancel_task`, `retry_cleanup` |
| `land` | `land_task`, `discard_task` |

The phone's default is `read` + `answer`. `control` and `land` are granted per client, on the
host. `pick_project` is host-local only (it opens a native dialog) and is never remote. A call
outside the client's scope returns `error{code: forbidden}` and is logged on the host: the
method and the client, never the payload.

**Reopened if** the owner wants one trust level per client (a simpler UI). Then two levels:
`viewer` (read + answer) and `operator` (everything).

## Revocation

**Decided:** revoking a client deletes its key on the host.

- An open connection receives `error{code: revoked}` and is closed; its queued calls are
  dropped.
- Any later handshake with that key fails at the Noise layer and the socket closes with no
  message, indistinguishable from an unknown client.
- On `revoked` the client shows "Removed from <host>. Pair again." After a failed handshake
  it shows "Not paired with this host". On `revoked` it deletes its cached snapshot and its
  keys for that host. (A1 amends this: after a failed handshake it deletes nothing; see
  "What A1 decided", G6.)

**Reopened if:** no reversal condition was recorded at the gate. Any change to the revoked
or unknown-key behaviour reopens this section.

## Push

**Decided: A, no real push in v1.** Notifications arrive only while the phone is connected
(app open or recently backgrounded). A `register_push{token, platform}` call and a `push`
feature flag in `welcome` are **reserved**, so A3 can add C or B without a v2.

Not chosen: B, FCM through a relay we run (a server of ours, an ops item). C, FCM sent
directly by the host with the user's own Firebase credentials (a content-free wake-up, then
the phone connects). C does not scale past the owner, because the app's FCM sender key would
have to sit on every host.

**Reopened if** the owner wants asleep-phone alerts before A3. Then C for the owner's own
host; B only with an ops plan for the relay.

## Messages

Every message is JSON `{v: 1, type, ...}`.

**Envelope and version handshake.** Before the Noise handshake the client sends a plaintext
`{proto: "orchestra", v}`. The host answers `{proto, v, supported: [1]}`, or
`error{code: unsupported_version, supported}` and closes. Both plaintext frames are bound into
the Noise prologue, so they cannot be downgraded. Inside the channel every message still
carries `v`. An unknown `v` is a loud failure (error, then close), never a guess. An unknown
`type` at a known `v` gives `error{code: unknown_type}`, not a crash.

**Client to host.**
- `hello{v, client_name, app_version}`, sent after the handshake.
- `call{v, id, method, args}`, where `method` is a Tauri command name verbatim:
  `list_projects`, `list_sessions`, `list_inbox`, `get_transcript`, `list_tasks`,
  `get_task_diff`, `storage_status`, `start_session`, `stop_session`, `answer`, `create_task`,
  `cancel_task`, `land_task`, `discard_task`, `retry_cleanup`. `args` are the same serde
  structs, with `deny_unknown_fields`.
- `ping`.

**Host to client.**
- `welcome{v, host_id, host_name, scopes, features}`.
- `snapshot{v, seq, projects, sessions, inbox, tasks, storage}` on every connect. v1 does no
  replay.
- `event{v, seq, name, payload}`, where `name` is a Tauri event name verbatim:
  `session-updated`, `inbox-updated`, `transcript-appended`, `task-updated`. Plus `notice`,
  the core's notification decision, so `orchestrad` can forward notifications (H2/A3).
- `result{v, id, ok | error}`.
- `error{v, code, message}`.
- `pong`.

**Mirror rule.** The remote surface is the local one: the same method names, the same event
names, the same payload types as `desktop/src/ipc.ts` and `types.ts`. A new Tauri command is
not remote until this directory lists it with a scope.

**Idempotency.** `call.id` is client-generated. The host remembers recent ids per client, so
an `answer` resent after a dropped link (the A2 offline queue) applies once. An already
resolved inbox item returns `error{code: already_answered}`.

**Limits.** One decoded message is at most `MAX_MESSAGE_BYTES` = 4194304 (4 MiB) of UTF-8;
text fields keep `MAX_TEXT_BYTES` = 32768. Handshake attempts are rate-limited per source. Frames are never logged. Absolute
paths never cross the link (the existing IPC rule).

**Reopened if:** no reversal condition was recorded at the gate. Any change to the message
set, the handshake or the mirror rule reopens this section; the numbers are in `constants.ts`.

## Versioning

- An **optional field** in host output, or a **new event or feature flag**, does not bump `v`.
- **Clients ignore unknown fields** and render unknown enum values (agent kind, host, status,
  PR state, transcript entry kind) as plain text. A transcript entry whose `kind` is a string
  this build does not know decodes to `{kind: "unknown", raw_kind, text, seq, at}` (`text` is
  the string `text` of the entry, else `null`; other fields are dropped), so a new entry kind
  does not bump `v`. `unknown` is reserved and is never a host entry kind.
- **Host input stays strict** (`deny_unknown_fields`), so a new call argument is gated on a
  `welcome.features` flag.
- **Removing or changing the meaning of anything bumps `v`.**

**Reopened if:** no reversal condition was recorded at the gate. Any change to these
compatibility rules reopens this section.

## Room for later

Reserved now so a later milestone does not need a v2:

- **D7:** sessions and tasks carry `agent` (a string; `claude` today).
- **Host id:** `welcome` carries `host_id`, whatever D8 decides.
- **D8:** every snapshot and event is scoped to the host of that connection. Items gain an
  optional `host` only if D8 (SSH sub-hosts) survives.
- **D6:** tasks carry `pr: null | {state, url, checks}`.
- **H2:** the `notice` event, and a read path with no desktop-only call.
- **H3:** host key pinning, Unreachable-not-Failed, and one connection per host.

**Reopened if:** no reversal condition was recorded at the gate.

## Using it

No package, no workspace, no copy step: a consumer imports the files by relative path.
`protocol/` is erasable TypeScript with relative, extensionless imports and no construct that
Babel compiles to a `@babel/runtime` helper (no `class`, no iterable destructuring), because
it has no `node_modules` of its own.

- **Mobile** adds `../protocol` to `watchFolders` in `metro.config.js` (Expo watches only its
  own folder outside a workspace, and Metro will not resolve a file it does not watch). Its
  jest suite runs the same fixtures.
- **Desktop** does not import it at runtime yet; `src/protocol.test.ts` is the canonical
  fixture run and asserts that `src/types.ts` stays assignable to the wire types.
- **Rust** is `orchestra_core::wire`, tested against `fixtures/` by `core/tests/wire_fixtures.rs`.
- A **fixture case** is one file: `{name, dir, wire | raw, expect, client_only?, decoded_wire?}`.
  `dir` is `c2h`, `h2c` or the plaintext `hs_c2h`, `hs_h2c`. `expect` is `"ok"` (decode, then
  encode, gives back `wire`) or `{error: code}`. A `client_only` case holds a tolerance the
  Rust copy does not model (an unknown enum value, an extra field); Rust only checks that it is
  JSON. `decoded_wire` is what a lossy decode re-encodes to.

## What P2 decided

Wire types in code (`types.ts`, `codec.ts`), the Rust copy (`orchestra_core::wire`) and the
shared fixtures. Where the sections above left a gap:

- **Size.** `MAX_MESSAGE_BYTES` = 4194304 (4 MiB) for one decoded JSON message, checked
  first, before parsing. One snapshot carries every inbox item, and an item can hold several
  32 KiB text fields. Splitting a message across Noise frames (65535 bytes each) was left
  to P3, and is done there (see "What P3 decided").
- **Field types.** `v` is the integer 1. `call.id` is a non-empty string of at most 128
  bytes. `seq` is a non-negative integer; the codec does not check its order (the host
  assigns it in order in P3; a client checks it). `v` is read as the integer it denotes:
  `1.0` is `1` (see "What P3 decided"). Every
  string a client sends is at most `MAX_TEXT_BYTES`. `result` has exactly one of `ok`
  (any JSON, `null` for unit commands) and `error{code, message}`. The handshake frames are
  `{proto: "orchestra", v}` and `{proto, v, supported: [1]}`, with no `type`. The 15 methods
  above are the whole method set; `pick_project` is not one.
- **Error codes.** A closed set on the host: `unsupported_version`, `unknown_type`,
  `bad_request`, `forbidden`, `revoked`, `already_answered`, `failed`. `bad_request` covers
  malformed JSON, a missing or non-integer `v`, a wrong field type, an unknown field, an
  unknown method, an oversize message and an over-long string. `failed` is a command's
  error string, mapped in P3. A client keeps a code it does not know as plain text.
- **Tolerance.** Host input is strict: an unknown field is `bad_request`. Client input
  ignores unknown fields, keeps unknown enum values (status, kind, model, scope, feature,
  error code, transcript entry kind) as the strings they arrived as, reads an unknown event
  name as an unknown event rather than an error, and reads an unknown transcript entry kind as
  the `unknown` fallback rather than failing the event. An unknown `v` is
  `unsupported_version` in both directions, checked before `type`, `proto` and every field.
- **Not typed yet.** The `notice` payload (H2), `pr` (D6) and `register_push`. `agent` and
  `host` are declared optional on the payloads; the core does not emit them today.
- **No `ack`.** `result{id}` acknowledges a call.

Still open after P2: the choice between the two JS crypto options (A1) and the scope UI
(P3b).

## What P3 decided

The host side of the link, built in `orchestra_core::link` (P3a): listen, pair, authenticate,
stream a snapshot and events, and run calls. The desktop Devices UI and its OS keyring
provider are P3b (built; see below); the phone Noise is A1. Nothing starts listening unless a caller gives
`LinkConfig::binds` an address, so the link is off by default on every host.

- **Two paths, two handshakes.** The WebSocket path selects the mode. `/pair` runs
  `Noise_IKpsk2_25519_ChaChaPoly_SHA256` and upgrades only while a pairing window is open;
  `/link` runs `Noise_KK_25519_ChaChaPoly_SHA256`. Any other path, and `/pair` with no open
  window, gets a plain HTTP 404, so a closed window looks like a path that does not exist.
  The `{proto, v}` frames are unchanged (no mode field, no codec or fixture change).
- **Version exchange.** The client sends the plaintext `{"proto":"orchestra","v":1}` as one
  WebSocket **text** message; the host answers with its reply frame as one text message
  (`{proto, v, supported: [1]}`, or `error{code: unsupported_version, supported}` and then
  closes). Each Noise handshake message is one WebSocket **binary** message.
- **Prologue, exactly.** The bytes `orchestra-link v1` (ASCII, 17 bytes), then three parts in
  this order, each as a 4-byte big-endian length and its bytes: the path (`/pair` or
  `/link`), the client request frame, the host reply frame. The two frames are the exact
  text strings as sent and received, never re-serialised. The path and both frames are
  therefore bound into the handshake: swapping the path, or rewriting a frame to downgrade
  the version, fails it.
- **Framing.** After the handshake one application message is one WebSocket binary message
  holding a sequence of `[u16 big-endian length][Noise ciphertext]`. Each ciphertext is at
  most 65535 bytes, so each plaintext chunk is at most 65519 (65519 bytes is one chunk, 65520
  is two). A message of 0 bytes is one empty chunk. The receiver joins the chunks and refuses
  when the plaintext would pass `MAX_MESSAGE_BYTES`; the WebSocket layer cap is the matching
  ciphertext bound (65537 bytes per chunk), so memory is capped before anything is decrypted.
  A message that does not decrypt, is not UTF-8, or is too large closes the connection.
- **Pairing secret and offer.** The secret is 32 random bytes from the OS generator, used
  directly as the IKpsk2 psk: **256 bits**, which amends the 128 bits written under Pairing
  (a strengthening, and no KDF to mirror in JS). The offer a QR carries is JSON
  `{v: 1, host_id, host_name, addrs: ["ip:port"], host_key, secret, expires_at}`: `host_key`
  and `secret` are lowercase hex, `expires_at` is milliseconds since the Unix epoch.
  `host_id` is the hex of the first 16 bytes of SHA-256 of the host static public key.
  The window lasts 5 minutes and is single use: it closes when the client first transport
  message (its `hello`) decrypts, which is what proves the psk.
- **Confirmation on the host.** After the hello the core holds the connection and tells the
  host (`on_pairing_request`) the client name and fingerprint: the first 8 bytes of SHA-256
  of the client static key as lowercase hex in groups of four joined by `-`
  (`1a2b-3c4d-5e6f-7a8b`). `confirm_pairing` stores the client (default scopes `read` and
  `answer`, or the ones the host picks) and only then sends `welcome` and the snapshot.
  `reject_pairing`, the expiry of the confirm wait, or `stop` closes the socket with no
  message, the same as an unknown key.
- **Reconnect lookup.** A host does not know which paired client is calling before it reads
  Noise `KK` message 1, so it tries each stored client key (at most 32) and keeps the one whose
  tag verifies. No match closes the socket with no message. Handshakes are limited to 10 a
  minute per source address, which bounds the cost of the trial.
- **No message on failure.** A wrong psk, an unknown or revoked key and a rejected pairing
  all end as a closed socket. (A client with the wrong psk fails earlier still: it cannot open
  the second handshake message of the host, so it never sends a hello and the host never sees
  a request.)
- **Scopes.** The table under Authorisation, enforced in one place: after the call decodes
  and before it runs. A call outside the client scopes returns `error{code: forbidden}` and
  the host logs the method name and client id, never the arguments. Scopes are read from the
  store on every call, so `set_scopes` and `revoke` act on a live connection. An unknown scope
  string in `clients.json` grants nothing. The snapshot and events go only to a client holding
  `read`; a client without it gets `welcome` and may still make the calls it holds scopes for.
- **Idempotency.** The host keeps the last 256 call ids per client, in memory and across
  reconnects, for every call that is not a `read` call. A resent id returns the first result
  without running again; a resent id whose first run is still going returns `failed`. Read
  calls are not cached.
- **Errors from commands.** A command error is `failed` with the core own message, which has
  no path in it. The one exception is an `answer` whose inbox item is no longer pending
  (answered, resolved or unknown), which is `already_answered`. It is decided by the typed
  error of the core, never by the message text.
- **Snapshot and events.** `seq` is one process-wide counter starting at 1, assigned to every
  event as it is published, in publish order. `snapshot.seq` is the `seq` of the last event
  that snapshot already reflects; the first event after it has `snapshot.seq + 1`. A client
  applies events with a larger `seq` and checks they are consecutive. Each connection has a
  bounded queue (1024 events); one that falls behind is closed and the client reconnects for a
  fresh snapshot, because v1 has no replay. `notice` is not forwarded yet (H2).
- **Revocation.** `revoke` deletes the client key and its cached call ids. A live connection
  receives `error{code: revoked}` and is closed, queued calls are dropped, and the next
  handshake with that key fails with no message.
- **Limits and timeouts.** 16 connections, 32 paired clients, 8 calls in flight per
  connection (the ninth is `failed`, "busy"), 120 seconds without a message before the
  connection is closed (clients ping within 60), 30 seconds for a stuck write, 10 seconds for
  the version exchange and handshake. A newer connection from the same client replaces the
  older one.
- **Where things live.** All in a directory the caller names (`link/` under the app data
  directory for the desktop, the state directory for `orchestrad`): `host_key` (32 raw bytes,
  created once, mode `0600` on Unix, the per-user app data ACL on Windows) and `clients.json`
  (`{schema: 1, clients: [{id, name, public_key, scopes, paired_at}]}`, written atomically; a
  schema this build does not know, or a file that does not parse, stops the link from starting
  and the file is left alone). The desktop keeps its host key in Windows Credential Manager (P3b, built: a generic
  credential `com.telosrg.orchestra/link/host_key`, from the shell crate, never in core) and
  never reads or writes `link/host_key` on Windows; off Windows it uses the file. The file is
  the permanent `orchestrad` provider. The file is now written atomically: a unique temp (mode
  `0600` on Unix), synced, then published with a hard link that fails if `host_key` exists, so
  a crash leaves at most a stray temp and a second creator reads the first one's key.
  The desktop's own switch is `link/config.json` (P3b): `{schema: 1, enabled, port, binds,
  firewall_notice_seen}`, off by default, `enabled` persists and starts the link at launch, one
  fixed port (default 47800) for every bind, and a file it cannot read turns the link off and
  is left alone.
- **Binding.** Every address is explicit. `0.0.0.0` and `::` are refused before any socket is
  opened, with no override in v1. An empty list listens on nothing. Binding a loopback
  address never raises the Windows Firewall prompt; a LAN or Tailscale address can, once, for
  an executable with no inbound rule, so the desktop warns before it first enables one (P3b, built: the Rust command refuses the
  first non-loopback enable until the notice was shown). The desktop offers only addresses its
  adapters have now (IPv4: loopback, Tailscale `100.64.0.0/10`, private ranges), never free text.
- **The CSP bullet is moot.** Rust owns the socket in both roles, so the webview opens no
  connection and `connect-src` stays `ipc: http://ipc.localhost`.
- **`v` as a number.** An integral JSON number such as `1.0` is the integer it denotes for
  `v`, in both copies: JavaScript cannot tell `1.0` from `1` after `JSON.parse`, so the Rust
  codec existing leniency is the one consistent rule. (The note in the P2 open items that
  Rust rejects `1.0` was wrong.) One asymmetry remains: Rust `decode_host` reads `seq` as an
  unsigned integer and rejects `seq: 1.0`, where the TypeScript codec accepts it. It matters
  only to a Rust client (H3), since a Rust host never emits `1.0`.
- **Built in P3b** (changes to P3a's host, no wire change): a pairing client name is cut to
  128 bytes with control and bidirectional-formatting characters dropped before the host
  shows or stores it, so the confirm dialog and the paired list agree; and a confirm that
  lands as the connection's deadline passes is honoured instead of stored without a welcome
  (the connection takes its own pending entry, or waits up to a second for the decision the
  host already took).
- **Not in P3a, now built in P3b.** The desktop Devices view (QR, paired list, revoke,
  scope grants, the confirm dialog), the persisted enable flag, the keyring provider and the
  Tauri commands. **Still not built:** the phone Noise and its test vectors (A1); a Rust client (H3); `orchestrad` (H2);
  replay after a reconnect; TLS, a relay and mDNS.

## What A1 decided

The phone side of the link: pairing by QR, the Noise client and the first live view. This
section records the decisions slice s1 carries; the phone code follows in later slices. No
wire change: `v` stays 1 and the desktop is unaffected.

- **Noise on the phone (G1, amends Transport).** Hand-written, initiator-only Noise for
  exactly `Noise_IKpsk2_25519_ChaChaPoly_SHA256` and `Noise_KK_25519_ChaChaPoly_SHA256`, in
  `mobile/src/link/noise.ts`, over `@noble/curves` x25519, `@noble/ciphers`
  chacha20poly1305 and `@noble/hashes` sha256 and hmac. It has two hard-coded token
  sequences (IKpsk2: pre `<- s`; `-> e, es, s, ss`; `<- e, ee, se, psk`. KK: pre `-> s`,
  `<- s`; `-> e, es, ss`; `<- e, ee, se`), no responder, no rekey and no pattern parser.
  Keys come from an injected random function. The acceptance bar is byte-for-byte
  agreement with the snow transcripts below, not "the Noise test vectors" Transport named.
  **Reopened if** a maintained JS Noise library passes these vectors on Hermes (then switch
  to it), or a review finds a flaw in the hand-written one (then A2 stops until fixed).
- **Test vectors (G2).** `protocol/fixtures/noise/ikpsk2.json`, `kk.json` and `offer.json`
  are generated by the core, not by hand: `core/src/link/vectors.rs` (test only) builds both
  handshakes through the production `noise.rs`, with snow's fixed-ephemeral test hook behind
  one `cfg(test)` constructor. Keys are the SHA-256 of label strings. Each transcript
  holds: `suite`, `path`, the exact `request_frame` and `reply_frame` texts, `prologue_hex`,
  the static and ephemeral secret and public keys of both sides, `psk_hex` (IKpsk2 only),
  `msg1_hex`, `msg2_hex`, `handshake_hash_hex`, and `transport`: framed (`frame::seal`)
  messages `c2h_hello`, `h2c_welcome` and `c2h_ping` as plaintext and wire hex, `h2c_big` (a
  65520-byte plaintext, exactly two chunks) in full as base64, and `c2h_big` as the length and
  SHA-256 of its wire bytes. The big plaintext is byte `i` = `0x61 + (i mod 26)`. The two
  split keys are not recorded: snow exposes them only behind a feature the core does not
  enable, and the transport messages prove them. `offer.json` is `PairingOffer::to_json` for
  the vector host key, a fixed `expires_at`, and the addresses `127.0.0.1:47800` and
  `192.168.1.20:47800`. Each file is under 256 KiB, and `fixtures/noise/` is not read by the
  `fixtures/index.ts` glob.
- **Drift guard.** The core test `vectors_match_committed_files` rebuilds the files in memory
  and fails on any difference, so a change in snow, serde_json or the framing fails every
  core CI job. `ORCHESTRA_WRITE_VECTORS=1 cargo test -p orchestra-core --lib link::vectors`
  rewrites them; review the diff. A second test, `vectors_open_on_the_production_host_path`,
  feeds the committed message 1 to `responder_pair` and `responder_link`, checks the learned
  client key, and opens the committed client frames with `frame::open`.
- **Per-host client keys (G5).** The phone generates a fresh static key for each paired host,
  at pairing, so hosts cannot correlate the phone and forgetting one host deletes exactly its
  key. Storage is `expo-secure-store` only: `orchestra.hosts` (a JSON array of `host_id`) and
  `orchestra.host.<host_id>` (one record: `schema`, `host_id`, `host_name`, `host_key`,
  `addrs`, `client_secret_key`, `client_public_key`, `fingerprint`, `paired_at`). Each value
  is under 2048 bytes, `addrs` is at most 8, the record is written before its index entry, and
  no snapshot cache is persisted in A1.
- **What a silent close means (G6, amends Revocation).** The phone tracks the phase reached.
  The socket never opens: unreachable (on `/pair`, a close reason with 404: the code was used
  or the window closed). An error frame in the version exchange: version mismatch. `/link`
  closed after message 1 with no message 2, twice in a row with the network up: "Not paired
  with this host", with Pair again and Forget this desktop, retrying at the slow backoff.
  `/pair` message 2 does not decrypt: wrong code or host key, final for that QR. `/pair`
  closed after the hello and before `welcome`: rejected, timed out or the desktop stopped (the
  phone cannot tell which). `error{code: revoked}`: "Removed from <host>. Pair again." The
  phone deletes a host's record and key only on `revoked` or on Forget, never after a failed
  handshake. This replaces the Revocation line that said a failed handshake deletes keys.
- **Placement (G7).** `protocol/` gains data only (`fixtures/noise` and this section); no
  TypeScript is added there. Mobile keeps importing `protocol/` by relative path, and Noise
  and framing live in `mobile/src/link`, because `protocol/` has no `node_modules` for a
  bare `@noble` import. The vectors are the shared contract between the two Noise
  implementations.
- **Reopened if** a snow upgrade changes a transcript (regenerate, and the phone must still
  agree), or the framing or prologue changes (a wire change, so `v` moves).
