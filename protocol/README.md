# protocol/ — the desktop <-> phone contract

The message format between the desktop app (which runs the agents) and the Android app
(which remote-controls them). Both clients are built from this definition.

- **One protocol, versioned here.** Every message carries the protocol version.
- **A protocol change ships in both clients together.** A change that one client cannot
  speak is not merged until the other can. Desktop and mobile release on separate channels
  (installers vs. the Play Store), so the older client must fail loudly on a version it
  does not understand, never guess.
- The TypeScript definitions here are the source of truth; the Rust side in
  `desktop/src-tauri` keeps a matching copy, checked by tests.

Empty until the desktop<->phone link is designed.
