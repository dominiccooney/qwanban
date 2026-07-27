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

## TODO

- Computer use: mouse jumping for clicks but animation for moves, drags
- MCP wrapper
