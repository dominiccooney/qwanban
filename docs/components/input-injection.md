# Component: Computer-Use Backend / Input Injection (`qwanban-guest` :: computer module)

> Owns the **Anthropic computer-use tool backend**: it is the "tool executor"
> that runs the actions Cline emits via the built-in `computer_20250124` tool,
> plus the OS-level input + screenshot scaling that requires. Read
> [`README.md`](README.md) §S1–S8. Implements design.md §4.3 computer control
> and resolves open question §15.1.

## Purpose & scope

**Decision (from maintainer):** Cline is patched to send Anthropic's
**computer-use beta** (`betas=["computer-use-2025-01-24"]`, tool
`type="computer_20250124"`) using Anthropic's recommended resolutions and the
built-in schema. The computer-use tool is **schema-less and built into the
model** — *the application executes it client-side*. In qwanban, **this component
is that executor** (the equivalent of the reference `computer.py`), running
inside the guest as part of the qwan agent.

Responsibilities:

1. Implement every computer-use `action` (below) on **Windows and Linux**.
2. Own **screenshot capture for the tool** (delegating frame bytes to
   `video-capture-encode`'s `FrameSource`) and the **coordinate/resolution
   scaling** Anthropic requires.
3. Stamp each executed action with `injected_ts` (timeline_ns) for the transcript
   join (S2), and feed a `ToolIo` echo to the transcript.

It does **not** decide *what* to do (that's Cline/the model). It is *not* an MCP
tool (see mcp-server.md decision). Owns the `InputEvent`/`InputAck` types and the
`ComputerAction` mapping.

## Sequence coverage

Owns the executor in **7.4.1–7.4.13** (the whole computer-control loop:
screenshot, action dispatch, OS injection, ack, settle), **7.2 screen geometry**
reported in `GuestInfo`.

## Dependencies

- Upstream caller: the **Cline agent-loop adapter** in the qwan agent (it hands
  decoded `computer` tool_use blocks here and returns `tool_result` blocks with
  the screenshot). NOT the MCP server.
- `video-capture-encode` `FrameSource` for screenshot bytes.
- The guest-local `Timeline` (S2) for `injected_ts`.
- OS APIs (no broker dependency; pure local).

## Anthropic computer-use action set (the contract we implement)

From `computer_20250124` (superset of `20241022`). The model sends `{action,
coordinate?, start_coordinate?, text?, scroll_direction?, scroll_amount?,
duration?, key?}`:

```
screenshot                         # capture display -> base64 image (scaled, see below)
left_click {coordinate, key?}      # key = modifier(s) held during click
right_click | middle_click {coordinate, key?}
double_click | triple_click {coordinate, key?}
left_click_drag {start_coordinate, coordinate}
left_mouse_down | left_mouse_up    # fine-grained (no coordinate)
mouse_move {coordinate}
cursor_position                    # report current pos (scaled to API space)
scroll {coordinate, scroll_direction, scroll_amount, text?(modifiers)}
type {text}                        # UTF-8 string
key {text}                         # chord, xdotool-style e.g. "ctrl+s", "Return"
hold_key {text, duration}          # hold for duration seconds
wait {duration}                    # pause, then screenshot
```

We expose `display_width_px`, `display_height_px`, `display_number` in the tool
params — set to the **scaled** target resolution (below), per the reference impl.
(Process launch is **not** a computer-use action; the SUT is started by the qwan
agent at case setup or by the agent via keyboard/UI, or via a separate qwan MCP
tool if needed — keep launch out of this backend.)

## Coordinate & resolution scaling (REQUIRED — matches reference impl)

Anthropic recommends not exceeding XGA/WXGA-class resolutions and scaling
coordinates between the model's image space and the real screen. We replicate the
reference `scale_coordinates` logic exactly:

- Scaling targets (pick the one matching the screen's aspect ratio, only scale
  **down**): `XGA 1024×768 (4:3)`, `WXGA 1280×800 (16:10)`, `FWXGA 1366×768
  (~16:9)`. Baseline default to advertise: **1280×720** unless the SUT needs more.
- `ScalingSource::COMPUTER` (screen→API): downscale screenshots to the target
  before returning to the model; advertise the target as `display_*_px`.
- `ScalingSource::API` (API→screen): scale the model's `coordinate` **up** to
  real screen pixels before injecting; reject out-of-bounds coordinates.
- Preserve aspect ratio; never distort. (Avoids the "clicks land near but miss"
  failure mode.) Configurable per image (`capture.scale_target`).

## Types (owner)

```rust
pub struct InputEvent {              // internal normalized event after scaling
    pub event_id: String,            // evt_…
    pub kind: InputKind,
}
pub enum InputKind {
    MouseMove { x: i32, y: i32 },
    MouseButton { x: Option<i32>, y: Option<i32>, button: Button, action: BtnAction },
    Drag { x1:i32,y1:i32,x2:i32,y2:i32 },
    Scroll { x:i32, y:i32, dir: ScrollDir, amount: u32, mods: Vec<Key> },
    TypeText { text: String },               // UTF-8; chunked + paced
    KeyChord { combo: String },              // xdotool-style, as the model sends
    HoldKey { combo: String, duration_s: f32 },
}
pub enum BtnAction { Down, Up, Click, DoubleClick, TripleClick }
pub struct InputAck { pub event_id: String, pub injected_ts: i64, pub ok: bool }
pub enum Button { Left, Right, Middle }
pub enum ScrollDir { Up, Down, Left, Right }
```

`Key`/`combo` follow the xdotool keysym vocabulary the model emits (e.g.
`ctrl+s`, `Return`, `Page_Down`); the per-OS backend maps them to real keys.

## Platform backends

A `trait InputBackend` with two implementations selected at compile/runtime:

### Windows backend

- Mouse/keyboard via **`SendInput`** (Win32 `INPUT` structs). Absolute pointer
  positioning normalized to `0..65535` against the virtual screen.
- Unicode text via `KEYEVENTF_UNICODE` (avoids layout dependence); `KeyChord`
  via virtual-key codes with modifier down/up bracketing.
- Window/app launch via `CreateProcessW`. Screen geometry via
  `GetSystemMetrics`/`EnumDisplayMonitors` (reported in `GuestInfo`).
- Note: the SUT must be on the **interactive desktop/session** the qwan agent
  runs in (Session 1). Document the requirement that the base image auto-logs-in
  a user and runs qwan-guest in that session (UIA/SendInput need an interactive
  desktop).

### Linux backend

- Preferred: **`uinput`** (kernel virtual input device) — works under both X11
  and Wayland compositors, injects at the evdev layer. Requires the bootstrap to
  grant `/dev/uinput` access (documented image requirement).
- Fallback for X11-only images: **XTEST** (`x11rb`/`xdotool`-style) when a uinput
  device can't be created.
- Coordinates: uinput needs absolute axes setup (`ABS_X/ABS_Y` ranges = screen
  size); for XTEST, use `XWarpPointer`+`XTestFakeButtonEvent`.
- Text: map UTF-8 to keysyms (handle layout); for uinput, synthesize keystrokes,
  or use compositor text-input where available.
- Screen geometry from the compositor/X server; reported in `GuestInfo`.

## Behavior contract

- **Synchronous-ish:** executing an action performs the OS call and returns
  `InputAck` with `injected_ts` (timeline_ns) stamped immediately after the call
  returns (the join point the video shows).
- **Click semantics:** `double_click`/`triple_click`/`left_mouse_down`/
  `left_mouse_up` map to the right OS down/up sequences; modifier `key` held
  during click/scroll is bracketed (keydown … action … keyup), matching the
  reference impl.
- **TypeText chunking & pacing:** chunk long text (reference uses ~50-char groups
  with a ~12ms inter-key delay) so the SUT keeps up; this backend owns the
  chunking (there is no MCP layer in front of it).
- **No retry policy:** if an OS call fails, return `ok:false` + `QwanError`
  (`Internal`); the model decides whether to retry.

## Interfaces (exported)

```rust
/// Executes one Anthropic computer-use action end-to-end:
/// scales coords (API->screen), injects, captures+scales screenshot (screen->API),
/// returns the tool_result content the agent loop sends back to Claude.
pub trait ComputerUseExecutor: Send + Sync {     // consumed by the agent-loop adapter
    async fn execute(&self, action: ComputerAction) -> Result<ToolResult>; // {image?, text?}
    fn advertised_resolution(&self) -> Resolution;  // scaled target -> tool params + GuestInfo
}
/// Lower-level seam for unit testing (per-OS backend behind it).
pub trait InputBackend: Send + Sync {
    async fn inject(&self, ev: InputEvent) -> Result<InputAck>;
    fn screen_geometry(&self) -> ScreenGeometry;
}
```

### Local endpoint (for the "in-agent" wiring)

When the launched agent owns its own loop (agent-lifecycle Q2 mode 1), `cuxec`
also exposes a **loopback endpoint** at `QWAN_CUXEC_ADDR` (e.g.
`127.0.0.1:<port>`, HTTP or stdio) that accepts an Anthropic `computer` action
JSON and returns the `tool_result` (image/text). This is just `execute()` over
loopback; same validation/scaling. For the "qwan-driven loop" mode (mode 2) the
adapter calls `execute()` in-process and no endpoint is needed.

## Testing

- **Unit (mock `InputBackend`):** action→event mapping; **coordinate scaling**
  round-trips (API→screen→API) for each XGA/WXGA/FWXGA target incl. out-of-bounds
  rejection (mirror the reference `scale_coordinates` test vectors).
- **Unit:** `type` chunking/pacing; modifier bracketing for click+key and
  scroll+key; double/triple/down/up sequences.
- **Windows integration (gated):** launch Notepad, `type` text, `key ctrl+a`,
  screenshot via capture pipeline shows the text — closes the 7.4 loop.
- **Linux integration (gated):** same against a simple GTK/X app under both a
  uinput and XTEST path.
- **Geometry:** multi-monitor/DPI scaling correctness (coords map to the same
  pixel the screenshot showed).

## Open items

- Wayland text-input edge cases (some compositors restrict synthetic input) —
  uinput is the mitigation; document any compositor allowlist needed in base
  images.

> **DECIDED (Q4): vision only.** qwan exposes screenshots + coordinate input and
> nothing else — **no accessibility/automation-tree integration** (no UIA on
> Windows, no AT-SPI on Linux) and no window-enumerate/find_text helpers. If the
> agent wants an a11y/automation tree, it derives it **on-device** with its own
> tooling. This keeps `cuxec` OS-agnostic and matches the Anthropic computer-use
> surface 1:1.
