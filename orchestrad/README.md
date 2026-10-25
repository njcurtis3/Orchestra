# orchestrad

The Orchestra headless host: the same engine and phone link as the desktop app, run as a
Linux service with no window. A phone pairs to it and answers its agents.

**Nothing installs this for you.** There is no package, no installer and no script; this is
a by-hand guide, and packaging is a later milestone (R3). Run the steps below as root on
the machine that will host the agents. The files referred to are in this directory:
`systemd/orchestrad.service`, `systemd/gpu.conf.example` and `config.example.toml`.

## Before you start

- A Linux box with systemd and `claude` (Claude Code) installed where the `orchestra` user
  can run it, for example `/usr/local/bin/claude`. orchestrad looks it up on `PATH`.
- `git`, if you will run tasks (they use worktrees).
- The `orchestrad` binary, built with `cargo build --release -p orchestrad` from the repo
  root, copied to `/usr/local/bin/orchestrad` (mode 0755, owned by root).

## Install

```bash
# A system user with no login. Its home is the state directory.
useradd --system --home-dir /var/lib/orchestra --shell /usr/sbin/nologin orchestra

# Where project folders live by default. The unit can write here and nowhere else.
install -d -o orchestra -g orchestra -m 0700 /var/lib/orchestra /var/lib/orchestra/projects

# Config: root-owned, readable by the daemon.
install -d -o root -g root -m 0755 /etc/orchestrad
install -o root -g orchestra -m 0640 config.example.toml /etc/orchestrad/config.toml
$EDITOR /etc/orchestrad/config.toml

# The unit.
install -o root -g root -m 0644 systemd/orchestrad.service /etc/systemd/system/orchestrad.service
systemctl daemon-reload
```

Edit the config before you start. `binds` is the address the phone connects to (a LAN or
Tailscale IPv4 address, never `0.0.0.0`); with no `binds` the link stays off and the daemon
listens on nothing. Each `[[projects]]` path must be an absolute directory owned by the
`orchestra` user (`chown -R orchestra:orchestra <dir>`), or it is logged and skipped. A bad
value, an unknown field or an unknown `schema` stops the daemon at start and names the field.

The unit lets the daemon write only under `/var/lib/orchestra`. For a project folder
elsewhere, add a drop-in (`systemctl edit orchestrad`):

```ini
[Service]
ReadWritePaths=/srv/code
```

## Sign in to Claude

orchestrad does not sign in for you. It runs `claude`, and `claude` needs credentials. Pick one.

- **A token (suits a server).** On a machine with a browser, run `claude setup-token` and
  copy the token it prints. Then put it in the env file, root-owned and unreadable by anyone else:

  ```bash
  install -o root -g root -m 0600 /dev/null /etc/orchestrad/env
  echo 'CLAUDE_CODE_OAUTH_TOKEN=<the token>' >> /etc/orchestrad/env
  ```

- **An API key.** The same file, with `ANTHROPIC_API_KEY=<key>` instead.
- **A login on the box.** Sign in as the service user so credentials land in
  `/var/lib/orchestra/.claude/.credentials.json`, for example
  `sudo -u orchestra env HOME=/var/lib/orchestra claude` and follow the prompt.

At start the daemon checks that `claude` is on `PATH` and that one of these exists. If not,
it keeps running, logs an error that names `claude setup-token` and the env file, and
`orchestrad status` shows it. It never logs a secret value.

## Start

```bash
systemctl enable --now orchestrad
journalctl -u orchestrad -f          # logs; client ids only, never keys or secrets
sudo -u orchestra env RUNTIME_DIRECTORY=/run/orchestrad orchestrad status
```

`status` prints the version, state directory, listening addresses, paired phones, whether
pairing is open, the sign-in check, projects and session counts. It exits 3 when the daemon
is not running. The daemon refuses to run as root, and refuses to run as PID 1: under
Docker, use `--init`.

## Pair a phone

The control socket is `$RUNTIME_DIRECTORY/control.sock`, mode 0600, and only the daemon's
own user may talk to it, so run the commands as `orchestra`:

```bash
sudo -u orchestra env RUNTIME_DIRECTORY=/run/orchestrad orchestrad pair
```

It prints a QR code on your terminal only. Scan it in the Orchestra app. The terminal then
shows the phone's name and fingerprint; check it is your phone, and answer `y` to confirm
or `N` to reject. Ctrl-C closes the pairing window. The same two environment words apply to:

- `orchestrad clients` lists paired phones.
- `orchestrad revoke <id>` removes one.

Keep the terminal private while the QR is on screen: it carries the pairing secret.

## Optional: GPU access

The unit sees no devices (`PrivateDevices=yes`). To open them, copy
`systemd/gpu.conf.example` to `/etc/systemd/system/orchestrad.service.d/gpu.conf`, run
`systemctl daemon-reload && systemctl restart orchestrad`. Every agent can then use the GPU.
This only opens the device nodes; GPU workloads are planned work (H4).

## Security notes

- **Keep it off the public internet.** The link is encrypted and each phone is confirmed by
  you, but the port is meant for a LAN or Tailscale. Do not forward it from a router and do
  not bind a public address.
- The unit runs as an unprivileged user with an empty capability set and a strict
  filesystem sandbox. It deliberately leaves `MemoryDenyWriteExecute` and
  `RestrictNamespaces` unset: Node's JIT needs writable-executable memory, and Claude Code's
  own bash sandbox may need user namespaces. Confirm on a real box before tightening either.
- Stopping the service kills every agent process (`KillMode=control-group`).
- Agents run commands as the `orchestra` user. What that user can reach is what an agent can reach.
