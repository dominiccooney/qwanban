## Running qwanban

Install qbt from this repository (the crates.io package named `qbt` is an
unrelated qBittorrent client):

```powershell
cargo install --git https://github.com/dominiccooney/qwanban.git qbt
```

Run the server (agent port, then observatory WebSocket port):

```powershell
cargo run -- serve 1234 5678
```

`serve` retains the newest 200 screenshots as encoded PNGs. Configure the
capacity and the root for saved artifacts at startup:

```powershell
cargo run -- serve 1234 5678 --max-screenshots 200 --artifact-root runs
```

The startup log estimates the buffer's memory from a fresh PNG sample. Both
settings take effect when the server starts.

qbt keeps the newest 500 events in an in-memory journal: it
records every computer action it executes (with a full-screen screenshot for
screen-capturing actions), and the agent publishes transcript and status
events into the same journal via the `publish_event` action. Observatory
clients connect to the WebSocket port and receive the journal — a snapshot,
then live events, in one order — and fetch screenshots by id. One agent
connection is served at a time; a newly connecting agent replaces the
previous one, so a restarted CLI can always reconnect.

Every successful action response containing `image` also contains the
`screenshot_id` assigned to the same PNG in the journal. Existing clients can
ignore this additive field. The agent can retain a selected frame with:

```jsonc
{ "id": 7, "action": "save_screenshot", "screenshot_id": "shot_123", "path": "screenshots/a.png" }
```

The response contains `saved` with the absolute destination. Relative paths
resolve under `--artifact-root`; parent (`..`) segments, symlink escapes, and
absolute paths outside that root are rejected. An ID that is no longer in the
buffer returns an error with `"error": "screenshot evicted"`.

Text clipboard actions use the native Windows, X11, or macOS clipboard:

```jsonc
{ "id": 8, "action": "get_clipboard" }
{ "id": 9, "action": "set_clipboard", "text": "hello" }
```

`get_clipboard` returns `text` as a string or `null`; `set_clipboard` returns
the normal empty success response.

Create an animated WebP from an inclusive retained range through the
observatory WebSocket (default `ws://127.0.0.1:5678`):

```powershell
cargo run -- flipbook --from shot_10 --to shot_20 --out runs/interaction.webp --fps 2
```

Frames follow journal order. Encoding runs on demand at lossy quality 75; no
background encoder remains active.

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

Optionally set `CLINE_COMPUTER_USE_BACKEND_COMMAND` in the terminal that launches Cline. Cline probes qbt and launches the command when needed before its first display query; helper mode also adds the driver's `computer_user_restart_backend` recovery tool:

```powershell
$Env:CLINE_COMPUTER_USE_PORT = '1234'
$Env:CLINE_COMPUTER_USE_BACKEND_COMMAND = 'qbt serve 1234 5678'
```

The agent port must match `CLINE_COMPUTER_USE_PORT`; `5678` is the observatory port. The command runs on the Cline host using `cmd.exe` on Windows or `/bin/sh` on Unix, from Cline's workspace directory and with its environment. Installing qbt on PATH makes executable lookup independent of that directory. Quote checkout-local executable paths containing spaces; do not put PowerShell-only syntax in the command unless it explicitly launches PowerShell.

Restart Cline after changing these variables. Keep the launch command in the foreground so Cline can clean up its own child; it leaves independently started backends running. If startup fails, run the command in a terminal to inspect its output, which Cline otherwise discards.

## Brief pause action

The agent protocol accepts `{"action":"brief_pause"}`. It waits for a fixed
300 ms and returns a screenshot. Agents can put it in `run_sequence` after an
action that opens a dialog, launcher, menu, or other transient UI and before
typing into that UI.

The internal `{"action":"shutdown_backend"}` recovery request acknowledges
the caller, then closes the agent and observatory listeners and exits qbt
cleanly. Cline uses it for explicit forced restart when qbt is still running
but screen capture has degraded.

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
