## Running qwanban

Run the server (agent port, then observatory WebSocket port):

```powershell
cargo run -- serve 1234 5678
```

qbt keeps an in-memory journal of everything observable on the host: it
records every computer action it executes (with a full-screen screenshot for
screen-capturing actions), and the agent publishes transcript and status
events into the same journal via the `publish_event` action. Observatory
clients connect to the WebSocket port and receive the journal — a snapshot,
then live events, in one order — and fetch screenshots by id. One agent
connection is served at a time; a newly connecting agent replaces the
previous one, so a restarted CLI can always reconnect.

Run the observatory:

```powershell
cd observatory
bun run dev
```

Then add a host by its WebSocket address (e.g. `localhost:5678`). The grid
shows each host at a glance (latest activity and screenshot); open a host to
flip between the driver transcript, the computer user transcript, and the
raw computer actions, with the screen as it was at any selected moment.

Get https://github.com/cline/cline branch dpc/computer-use, and:

```powershell
bun install
bun build:sdk
cd apps/cli
$Env:CLINE_COMPUTER_USE_PORT=1234
$Env:CLINE_HUB_PORT=5555
$Env:CLINE_COMPUTER_USER_MODEL = "claude-sonnet-5"
bun run dev
```

Then, you must use the Anthropic provider (*not* merely Anthropic models through the Cline provider, because those lack
the computer-use beta header.)

### Let Cline recover a stopped backend

Optionally set `CLINE_COMPUTER_USE_BACKEND_COMMAND` in the terminal that launches Cline. It adds the driver's `computer_user_restart_backend` tool, which probes qbt and launches the command only if qbt is unreachable:

```powershell
$Env:CLINE_COMPUTER_USE_PORT = '1234'
$Env:CLINE_COMPUTER_USE_BACKEND_COMMAND = 'C:\Users\User\clients\cline\qwanban\target\debug\qbt.exe serve 1234 5678'
```

Replace the absolute path with your built qbt executable. The agent port must match `CLINE_COMPUTER_USE_PORT`; `5678` is the observatory port. The command runs on the Cline host using `cmd.exe` on Windows or `/bin/sh` on Unix, with Cline's working directory and environment. Quote executable paths containing spaces; do not put PowerShell-only syntax in the command unless it explicitly launches PowerShell.

Start qbt yourself before starting Cline: this tool provides recovery, not automatic startup. Restart Cline after changing these variables. Keep the launch command in the foreground so Cline can clean up its own child; it leaves independently started backends running. If startup fails, run the command in a terminal to inspect its output, which Cline otherwise discards.

## TODO

- Computer use: mouse jumping for clicks but animation for moves, drags
- MCP wrapper
