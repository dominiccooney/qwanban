# Component: qwan MCP Server (`qwanban-guest` :: mcp module)

> Owns the **MCP tool surface** the Cline agent uses to drive the guest. Runs
> inside the VM on loopback only. Read [`README.md`](README.md) §S1–S8.
> Implements design.md §4.3 (qwan MCP), §9.1 (agent carries out steps).

## Purpose & scope

> **Decision (computer control is NOT MCP).** Cline is patched to use
> **Anthropic's built-in computer-use tool** (beta header
> `computer-use-2025-01-24`, tool type `computer_20250124`). That tool is
> *schema-less and built into the model* — the application (here, the qwan agent
> acting as Cline's tool executor / agent-loop backend) executes the actions.
> So **computer control does not flow through MCP**; it flows through the
> **computer-use backend** (owned by `input-injection` + `video-capture-encode`).
> See `input-injection.md` "Anthropic computer-use action set".

The qwan **MCP server** therefore exposes only the **qwan-specific** tools the
computer-use schema does *not* cover:

- **evidence:** `breadcrumb`, `clip`
- **handoff/finish:** `request_intervention`, `request_os_migration`, `finish`

It is a thin layer: validate/normalize → dispatch to `breadcrumbs-transcript`
(breadcrumbs/clips) and the qwan supervisor (handoff/finish). It contains no
OS input code and no encoder.

> Why split this way: the computer-use tool is high-frequency, latency-sensitive,
> and its schema/coordinate-scaling is dictated by Anthropic — keep it on the
> native path. Breadcrumbs/clips/handoff are low-frequency qwan concepts with no
> Anthropic equivalent — expose them as ordinary MCP tools the agent calls
> alongside computer use.

## Sequence coverage

Owns the `mcp` participant in: **7.2.7–7.2.8** (handshake/list_tools, qwan tools
only), **7.5.1, 7.5.4–7.5.5** (breadcrumb tool), **7.7.1, 7.7.9–7.7.10** (clip
tool), **7.10.1, 7.11.1, 7.12.1** (handoff/finish tools). The computer-control
steps **7.4.x** are owned by the computer-use backend (input-injection +
video-capture-encode), *not* this doc.

## Dependencies

- In-process: `breadcrumbs-transcript` (`BreadcrumbSink`), qwan supervisor
  (`HandoffSink`, `FinishSink`).
- Protocol: an MCP server library (Rust MCP SDK / `rmcp`-style). Transport:
  stdio or loopback TCP/WebSocket per what the patched Cline expects
  (config `cline.mode`).

## Transport & security

- Binds **127.0.0.1:<port>** (or stdio pipe) — never the case NIC. The Cline
  process is the only client; no auth needed beyond loopback isolation.
- Tool handlers are bounded/validated (length caps on labels, etc.).

## Tool catalog (owner of these schemas)

> Computer-control tools (`screenshot`/`*_click`/`type`/`key`/`scroll`/…) are
> **deliberately absent** — they are the Anthropic computer-use tool, executed by
> the computer-use backend (input-injection), not MCP tools.

### Evidence

```jsonc
breadcrumb({label, kind?, data?})  -> { ok, breadcrumb_id, timeline_ns }   // 7.5
clip({from, to, label})            -> { ok, clip_id, web_url }             // 7.7 (awaits clip_ready)
```

- `from`/`to` accept a `breadcrumb_id`, the sentinel `"now"`, or a relative
  `"-30s"`. The server resolves them to breadcrumb ids/timestamps before calling
  `BreadcrumbSink::make_clip`.
- `kind` enum (shared with breadcrumbs-transcript): `StepBegin`, `StepEnd`,
  `Action`, `Assert`, `ReproConfirmed`, `FixVerified`, `Note`, `Error`.

### Handoff / finish

```jsonc
request_intervention({reason})            -> { ok, held: true }       // 7.10
request_os_migration({target_os, reason}) -> { ok, migrating: true }  // 7.11
finish({result, summary, pr_url?, clip_ids?}) -> { ok }               // 7.12
```

- These call the qwan supervisor, which performs the broker `Handoff`/
  `ReportResult`. `finish` is **idempotent** and terminal: after it, further tool
  calls return `FailedPrecondition`.

## Capabilities advertised at handshake (7.2.8)

`server_info` lists the qwan tools above + `os`. (Screen dimensions / scaling /
screenshots belong to the computer-use backend, not here.) The system prompt qwan
injects (agent-lifecycle 7.2.6) tells the agent how to use these qwan tools
*alongside* its native computer-use tool (e.g. "emit a breadcrumb at each QA
step; cut a clip around any bug").

## Error mapping

Tool errors return MCP tool errors carrying `QwanError.code` (S5):
invalid `from/to` → `InvalidArg`; clip not ready yet → `Unavailable`; after
`finish` → `FailedPrecondition`.

## Interfaces (consumed)

```rust
trait BreadcrumbSink {
    async fn emit(&self, b: BreadcrumbIn) -> Result<Breadcrumb>;
    async fn make_clip(&self, from_ts: i64, to_ts: i64, label: String) -> Result<ClipAsset>;
    fn resolve(&self, bc_ref: BreadcrumbRef) -> Result<i64 /*timeline_ns*/>;
}
trait HandoffSink { async fn intervene(&self, r: String)->Result<()>;
                    async fn migrate(&self, os: String, r: String)->Result<()>; }
trait FinishSink  { async fn finish(&self, res: CaseResult)->Result<()>; }
```

(`BreadcrumbIn`/`Breadcrumb`/`kind` owned by breadcrumbs-transcript; `ClipAsset`
by artifact-store-and-clipping. This server does **not** consume `FrameSource`/
`InputSink` — those belong to the computer-use backend.)

## Testing

- **Unit:** schema validation (clamping, length caps, rate limits); `from/to`
  resolution for clips; idempotent/terminal `finish`.
- **Contract:** an MCP client harness lists tools and exercises each against
  mock sinks; asserts returned ids/timestamps are well-formed and monotonic.
- **Interplay:** with a mock `BreadcrumbSink`, a `clip` referencing two prior
  breadcrumb ids resolves to the right `timeline_ns` range and returns a
  `web_url`.

(The `screenshot→click→screenshot` loop is the computer-use backend's test, in
input-injection.md, not here.)

## Open items

- MCP transport (stdio vs loopback socket) depends on Cline form factor (link to
  agent-lifecycle open item).
- Whether `finish`/`request_*` should instead be modeled as Cline's native
  task-completion signal rather than MCP tools (depends on the Cline patch).
- ~~`find_text`/OCR helper~~ — **NO** (Q4: vision only; no host-side a11y/OCR
  helpers; the agent does any OCR/tree derivation on-device).

