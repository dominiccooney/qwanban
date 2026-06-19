# Component: Agent Lifecycle (`qwanban-guest` runtime + `qwanban-core`)

> Owns: the per-case push of the qwan agent, file injection, agent boot &
> registration, launching the Cline agent, handoffs, and finalize/teardown
> coordination. Also owns `JobSpec`, the manifest, `JobOutcome`, and the **case
> state machine** (in `qwanban-core`). Read [`README.md`](README.md) §S1–S8.
> Implements design.md §5.4, §5.5, §5.7, §12, §4.3.

## Purpose & scope

Two halves of one lifecycle:

- **Host side (`qwanban-core`):** the case state machine, the manifest builder,
  and the bootstrap orchestration (push → write files → launch) over the
  hvsocket transport (from hyperv-driver).
- **Guest side (`qwanban-guest` main):** the qwan agent's boot path — read
  manifest, register with broker, start subsystems (MCP, capture, transcript),
  spawn and supervise the Cline agent, route handoffs and the final result.

It does **not** implement the MCP tools, input, video, or transcript internals —
it *starts and supervises* those subsystems (separate docs) and wires them to the
broker.

## Sequence coverage

Owns: the **protocol** over hvsocket in **7.1.13–7.1.16** (push/write/launch),
**7.2.1, 7.2.4–7.2.6** (boot + spawn Cline), **7.2.9 producer**, the guest side
of **7.10.1–7.10.3 / 7.11.1–7.11.4** (handoff plumbing), **7.12.1–7.12.3**
(finalize/flush), and the host state transitions **7.1.17, 7.12.7**.

## Dependencies

- Host: hyperv-driver (hvsocket transport, VM ops), broker-protocol (OpenCase,
  transitions).
- Guest: broker-protocol client, mcp-server, video-capture-encode,
  breadcrumbs-transcript (started as subsystems).

## The manifest (owner of `Manifest` type)

Built by `qwanban-core` at 7.1.4, written into the VM at 7.1.15:

```jsonc
// manifest.json  (qwanban internal format — NOT the QA script format)
{
  "schema": "qwan.manifest/v1",
  "job_id": "job_…", "case_id": "case_…",
  "kind": "ScriptedQa | BugFix",
  "task": {
    "script_text": "…markdown…",   // for ScriptedQa  (human-readable, §9.1)
    "report_text": "…",            // for BugFix
    "note": "passes at v1.2.0, fails at main — find the bad commit"
  },
  "repo": { "url": "github.com/org/app", "ref": "main", "checkout_path": "/work/app" },
  "broker": { "endpoint": "https://10.0.75.1:7443", "cert_spki_sha256": "…" },
  "auth": { "case_token_file": "/qwan/case.token" },
  "inference": { "base_url": "https://10.0.75.1:7444/v1", "dummy_key": "DUMMY",
                 "allowed_models": ["qwen2.5-coder-32b", "…"] },
  "proxy": { "https_proxy": "http://10.0.75.1:8080", "ca_fpr_sha256": "…" },
  "agent": {
    // qwanban does NOT care what the agent form factor is (CLI/SDK/patched
    // binary). It drops files and runs a command. The maintainer defines this
    // per base image; qwanban only fills in env + writes the task files.
    "files": [ { "src": "<host path>", "dest": "/qwan/agent/...", "mode": "0755" } ],
    "launch": { "shell": "pwsh | zsh | bash | cmd", "command": "…", "cwd": "/qwan/agent",
                "env": { "OPENAI_BASE_URL": "$inference.base_url",
                         "OPENAI_API_KEY": "DUMMY",
                         "QWAN_MCP_ADDR": "127.0.0.1:$mcp_port",
                         "QWAN_TASK_FILE": "/qwan/task.md",
                         "ANTHROPIC_BETA": "computer-use-2025-01-24" } }
  },
  "capture": { "fps": 5, "segment_seconds": 4, "encode_where": "guest|host" },
  "limits": { "max_runtime_s": 2700 }
}
```

Secrets rule (S7): the manifest contains only **dummy** keys + the case_token
(itself worthless off-host). Real keys never appear.

> **Decision (Q2): agent = "files + a command".** qwanban is agnostic to the
> Cline form factor. The qwan agent (a) writes the task files (the QA script /
> bug report as `/qwan/task.md`, plus the qwan MCP config), (b) materializes the
> `agent.files` the maintainer specified, and (c) runs `agent.launch.command`
> under `agent.launch.shell` with the injected env. Whatever that command starts
> (a patched Cline CLI, an SDK harness, a script) is the maintainer's choice. The
> only hard requirements on that command:
> 1. it connects to the **qwan MCP** at `QWAN_MCP_ADDR` for breadcrumb/clip/
>    handoff/finish;
> 2. its model calls go to `OPENAI_BASE_URL` with `OPENAI_API_KEY=DUMMY`;
> 3. it drives the SUT via **Anthropic computer-use**, executed by `cuxec` — see
>    "Agent ↔ computer-use wiring" below.

## Host: bootstrap orchestration (7.1.13–7.1.16)

Over the **hvsocket** channel (the only bootstrap transport — see
[`stub-loader.md`](stub-loader.md); no SSH), `qwanban-core` speaks the bootstrap
protocol to the in-image `qwan-stub`:

```
HELLO/AUTH                                          // case_bootstrap_secret
PUSH_AGENT { sha256, len } <bytes>               -> ACK{ok|hash_mismatch}
WRITE_FILE { path, mode, len } <bytes>           -> ACK   // manifest.json, case.token, proxy CA, agent.files
LAUNCH { command, shell, env, cwd }                -> ACK{pid}   // starts qwan-guest
```

- **Agent binary selection:** host picks the `qwan-guest` build matching guest
  `os/arch`; the stub verifies sha256 after transfer (7.1.14 / 7.1.E3). Retries N
  times then `Error`. This is the §5.7 "push, don't rebuild" path; binaries are
  small static Rust executables.
- **One transport, both OSes:** bootstrap is always hvsocket via `qwan-stub`
  (Windows AF_HYPERV / Linux AF_VSOCK). The protocol framing here is shared with
  stub-loader.md (that doc is the guest-side implementer).

## Guest: boot path (7.2.1–7.2.6)

1. Parse `manifest.json`; load `case_token`.
2. `Register` with broker (7.2.2); store `timeline_offset_ns` (presentation-only,
   usually 0), `allowed_models`, ingest URLs, `IngestLimits`. **No clock anchor
   or skew is exchanged** (S2).
3. Start the capture pipeline first so it establishes the case timeline origin
   `t0` (S2), then start the other subsystems, injecting the broker client + a
   shared `Timeline` handle (`now() -> timeline_ns = monotonic_now() - t0`):
   - transcript sink (breadcrumbs-transcript)
   - capture pipeline (video-capture-encode)
   - MCP server (mcp-server) — passes it handles to input + capture + transcript.
4. Clone the repo at `repo.ref` into `checkout_path` (guest-local; uses git via
   the system proxy with the dummy token).
5. **Materialize the agent and run it** (qwanban is form-factor agnostic — "files
   + a command"):
   - write the task files: `/qwan/task.md` (script_text/report_text + note) and
     the qwan MCP config;
   - drop `manifest.agent.files` to their dests;
   - run `manifest.agent.launch.command` under `…launch.shell`, with env filled
     in (`OPENAI_BASE_URL`, `OPENAI_API_KEY=DUMMY`, `QWAN_MCP_ADDR`,
     `QWAN_TASK_FILE`, `ANTHROPIC_BETA`, …).
   The launched process is whatever the maintainer chose (patched Cline CLI, an
   SDK harness, a script). qwanban supervises it as an opaque child process.
6. Begin heartbeats (7.2.9).

> **Agent ↔ computer-use wiring (owned here).** Computer-use is a built-in,
> client-executed Anthropic tool (not MCP), so *something on the guest must
> execute it*. qwanban provides the `ComputerUseExecutor` (`cuxec`,
> input-injection) and a small **local endpoint** the launched agent calls to run
> computer-use actions. Two supported wirings (maintainer picks per image):
> 1. **In-agent (SDK/patched):** the agent owns its own loop and calls a tiny
>    local `cuxec` HTTP/IPC endpoint (`QWAN_CUXEC_ADDR`) per action.
> 2. **qwan-driven adapter:** the qwan agent runs the Anthropic agent loop itself
>    and the launched "agent" is just the model client.
> Either way `cuxec` does the OS injection + screenshot scaling and echoes a
> `ToolIo` to the transcript (7.4). The endpoint contract is owned by
> input-injection (`ComputerUseExecutor`).

## Cline supervision

- The qwan agent **supervises** the Cline process: captures stdout/stderr into
  the transcript as `log` entries, detects exit, and enforces `max_runtime_s`
  (soft stop → ask Cline to finish; hard stop → kill + `ReportResult(ERROR)`).
- Idempotent `finish` (7.12.1): if Cline calls the MCP `finish` tool, that drives
  7.12; if Cline exits without `finish`, qwan agent synthesizes a result from
  exit status + last breadcrumbs.

## Handoffs (7.10 / 7.11)

- MCP `request_intervention`/`request_os_migration` call into the qwan agent,
  which sends `Handoff` (broker-protocol) and then **parks** (intervention) or
  **uploads portable state then exits** (migration). Portable state = repo
  working-tree diff + manifest + local breadcrumb table; pushed via
  `PushPortableState` (7.11.4).

## Case state machine (owner)

`qwanban-core` owns the authoritative state enum and transitions (design.md §12).
Transitions are driven by hyperv-driver events + broker transition callbacks:

```
submit → Rejected(ResourceExhausted)   # if no free slot — hard cap, NO queue (§5.8)
submit → Admitted → Provisioning → Booting → QwanAgentPushed → ClineAgentReady → Running
Running → {Completed|Failed|Error} → Teardown → Archived
Running → InterventionRequested → Held
Running → OsMigration → Provisioning(new case) → Running
Held → {Resumed→Running | Discarded→Teardown}
```

Admission is synchronous at `submit`: `qwanban-core` counts live cases; if
`>= max_concurrent_cases` it returns `QwanError{ResourceExhausted}` immediately
(no `Queued` state, no waiting). The caller retries later if it wants.

## Interfaces (exported)

- `qwanban-core`: `Qwanban::submit(JobSpec) -> JobHandle`,
  `JobHandle::await_completion() -> JobOutcome`, `events()` stream of breadcrumbs
  /state (matches design.md §11.2). `JobSpec`/`JobOutcome`/`Manifest` types.
- `qwanban-guest`: `main()` boot path; `Supervisor` over subsystems.

## Testing

- **Host unit:** manifest building per job kind; bootstrap protocol framing;
  push retry/hash-mismatch.
- **Guest unit:** boot path with a mock broker + mock subsystems; verify Cline
  spawned with correct env/MCP config; finalize synthesis when Cline exits.
- **End-to-end (with broker-protocol integration harness):** 7.1→7.2→…→7.12 with
  a fake Cline that emits a couple breadcrumbs + a clip + finish; assert
  `JobOutcome` carries report_url + repros.

## Open items

- **DECIDED (Q2):** qwanban is agent-form-factor agnostic — it drops
  `manifest.agent.files` and runs `manifest.agent.launch.command`. Remaining
  detail: finalize the two `cuxec` wiring modes (in-agent endpoint vs.
  qwan-driven loop) and which is the default base-image recipe.
- Whether `finish`/`request_*` are MCP tools or the agent's native completion
  signal (depends on the chosen agent recipe).

