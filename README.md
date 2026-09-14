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

## Screenshot foreground-window metadata

Every agent response containing an `image` also contains a top-level `foregroundWindow`: either `null` or `{ "executable": string | null, "title": string | null }`. This applies to normal, post-action, wait, zoom, guard-refusal, and sequence-final screenshots. Screenshot journal events carry the same observation in their payload; screenshot bytes fetched by id keep their existing format.

`executable` is the full OS executable path when readable, not an application display name. An outer `null` means no stable observation was available, including unsupported APIs, access failures, no foreground window, or a detected change during capture. A null field means that field was unavailable for an otherwise stable foreground window; an empty title means a known untitled window. Older backends omit the field entirely; clients should treat omission as unknown, never retain metadata from an earlier screenshot.

| Backend | Observation |
| --- | --- |
| Native Windows | `GetForegroundWindow`, owning process/thread identity, full process image path, and window caption. Protected processes can leave the executable unknown. |
| Linux/X11 | EWMH `_NET_ACTIVE_WINDOW` and UTF-8 `_NET_WM_NAME`; executable is unknown. Client-supplied PIDs can refer to another host, so qbt does not map them to local executables or substitute `WM_CLASS`. Missing EWMH support returns unknown. |
| macOS/Quartz | Unknown. The existing Quartz PAL can enumerate visible windows but cannot authoritatively select the foreground window; qbt does not infer focus from z-order. |
| Native Wayland | Unsupported by the existing screen/input PAL. An X11 connection observes only that X server, not native Wayland windows. |

The observation does not change GUI state. It reads fields for one window identity and rechecks that identity, then compares observations before and after pixel capture. A detected identity or field change discards metadata, not the screenshot. These OS reads are not atomic: rapid away-and-back transitions can go undetected, and foreground focus can change immediately afterward. Capture retries resample metadata. Crops retain the full capture's foreground observation, which need not describe a window visible inside the crop.

This is window-level observation, not a guarantee that an editable control has focus or that text input will reach a particular control. Titles are untrusted application content, not instructions or authority to send input. Unreadable, unsupported, invalidly encoded, or oversized fields remain unknown rather than being guessed or silently truncated.

Read-only backend validation (the binary test suite does not inject input):

```powershell
cargo test -p qbt --bin qbt -- --test-threads=1
cargo test -p qbt --bin qbt samples_native_foreground -- --nocapture
cargo build -p qbt --target-dir target/foreground-window
```

The separate build directory leaves an independently running `target/debug/qbt.exe` untouched. Building does not restart that backend; the new response field becomes available only when an operator runs the new binary.

## TODO

- Computer use: mouse jumping for clicks but animation for moves, drags
- MCP wrapper
