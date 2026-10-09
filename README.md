# Orchestra

A desktop command center and Android remote for the coding agents running on your own
computer. See which agents are working, which need you, and which are done; answer them
from your desk or your phone; and run larger work through an orchestrated graph of agents.

Status: scaffold. Nothing works yet.

| Part | What |
|---|---|
| `desktop/` | Tauri 2 app (Windows first). Runs and tracks agents, hosts the inbox. |
| `mobile/` | Expo app for Android. Remote control, notifications, answers. Agents never run on the phone. |
| `protocol/` | The message format between the two. |
| `fleet/` | Vendored orchestration model. Never edited by hand. |
