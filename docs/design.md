# qwanban — Design Document

> Status: Draft v0.1
> Owner: Maintainer
> Last updated: 2026-06-17

## 1. Overview

**qwanban** is a Rust tool that orchestrates ephemeral **Hyper-V virtual machines**
on a Windows host to perform QA-type *agentic* software development tasks. A
**Cline** agent runs *inside* an untrusted guest VM and drives the software under
test (SUT), accompanied by a small **qwan agent** (qwanban's own in-VM
companion). The Rust host program is **non-agentic** and acts as the trusted
**orchestrator and security boundary**.

There are two primary use cases:

1. **Scripted QA (with prompt-level bug bisection).** Given a set of QA scripts
   (human-readable text/Markdown describing things to click, type, navigate,
   assert, etc.), qwanban runs the Cline agent through the scripts inside a VM
   and produces a **QA report** with reproducible repros (video clips,
   transcripts, logs). Attributing a failure to a specific commit ("bisection")
   is handled by instructing the agent in the prompt — not a separate subsystem.

2. **Bug-report driven repro & fix.** qwanban ingests existing bug reports,
   spins up a VM, and tasks the Cline agent with reproducing the bug and then
   producing a fix (PR) — with screen-recorded evidence of both the repro and
   the verification of the fix.

The central design tenet is a **strong trust boundary**: the host is trusted and
holds all secrets; the guest is untrusted and holds only dummy credentials. All
privileged/external actions (real API keys, GitHub tokens, inference billing)
flow through host-mediated, pinned, audited channels.

## 2. Goals and Non-Goals

### Goals

- Orchestrate ephemeral Hyper-V VMs from a Windows host via CLI and a Rust API.
- Support **Windows and Linux guests**, defaulting to Linux, with the ability to
  migrate a case from Linux→Windows (and vice versa) on agent request.
- Keep the **host non-agentic**; agents only ever run inside untrusted VMs.
- Continuous **screen recording** synchronized to **breadcrumbs** in the agent
  transcript, compressed and archived for later inspection in a web UI.
- Allow agents to cut **clippings** between breadcrumbs to demonstrate
  bugs/fixes for PRs.
- Provide host- or cloud-backed **inference** to the GPU-less guests, routed
  through the host's **LM Studio** instance with a fixed model set.
- A set of **base images** the maintainer builds and registers **by file path**,
  pre-configured for the SUT.
- **Ephemeral by default**: VMs are destroyed when done, except when an agent
  requests human intervention — then the VM is preserved.
- A host **MITM HTTPS proxy** that pins outbound traffic to trusted hosts and
  rewrites dummy keys to real keys; extensible later with rate limits / abuse
  detection.
- **Fast qwan-agent iteration**: the in-VM qwan agent is pushed per-case (no
  image rebuild).
- **Host-configured resource caps** on VMs (CPU/RAM/disk/runtime/concurrency).
- **Commit attribution ("bisection")** is achieved by the agent at the prompt
  level, not as a dedicated host subsystem.

### Non-Goals (initial)

- No GPU virtualization / passthrough into guests.
- No agent logic on the host (host stays a deterministic orchestrator).
- No multi-host clustering / distributed scheduling (single Windows host first).
- Not a general-purpose CI system; it is QA + repro/fix focused.
- No support for hypervisors other than Hyper-V in v1.

## 3. Key Concepts & Terminology

| Term | Meaning |
|------|---------|
| **Host** | The trusted Windows machine running qwanban, Hyper-V, LM Studio, and the MITM proxy. |
| **Guest / VM** | An untrusted, ephemeral Hyper-V VM (Windows or Linux) where the agent runs. |
| **Cline agent** | The AI that performs the QA/fix work inside the guest, powered by **Cline** (Cline SDK, CLI, or extension). |
| **qwan agent** | qwanban's small in-VM companion process: emits breadcrumbs, handles handoffs, exfiltrates the transcript, controls recording, and hosts the **qwan MCP** server. Separate from the Cline agent. |
| **qwan MCP** | An MCP server (provided by the qwan agent) that exposes **computer control of the guest** (and breadcrumb / clip / handoff / intervention tools) to the Cline agent. |
| **Job** | A unit of work submitted to qwanban (a QA run, or a bug repro/fix). |
| **Case** | A single VM-bound execution of a job; a job may span multiple cases (e.g. after OS migration). |
| **Base image** | A maintainer-supplied VHD/VHDX file, pre-configured for the SUT, that qwanban points to as a clone parent. |
| **Breadcrumb** | A timestamped structured marker the qwan agent emits into the transcript, used to index the screen recording. |
| **Clipping** | A video segment cut between two breadcrumbs, used as evidence in reports/PRs. |
| **Broker** | The host-side service the guest talks to for all mediated operations. |

## 4. High-Level Architecture

```
                         ┌──────────────────────────────────────────────┐
                         │                 WINDOWS HOST (trusted)        │
  CLI / Rust API  ─────► │  qwanban-orchestrator                         │
                         │   ├─ Job scheduler & state machine            │
                         │   ├─ Hyper-V driver (VM lifecycle)            │
                         │   ├─ Broker (gRPC/HTTP over host-only vSwitch)│
                         │   │    ├─ Inference router ─► LM Studio / cloud│
                         │   │    ├─ Secret vault (real keys)            │
                         │   │    ├─ Git/PR proxy                        │
                         │   │    └─ Recording & breadcrumb ingest       │
                         │   ├─ MITM HTTPS proxy (key rewrite + pinning) │
                         │   ├─ Artifact store (recordings, clips, logs) │
                         │   └─ Web report server                        │
                         │   ┌───────────── Hyper-V ────────────────┐    │
                         │   │  Ephemeral VM (untrusted, Win or Linux)│    │
                         │   │   ├─ Cline agent (SDK / CLI / ext)    │    │
                         │   │   │     └─ uses qwan MCP ───┐          │    │
                         │   │   ├─ qwan agent             │          │    │
                         │   │   │    ├─ qwan MCP ◄────────┘          │    │
                         │   │   │    │   (computer control, clips,   │    │
                         │   │   │    │    breadcrumbs, handoff)      │    │
                         │   │   │    ├─ transcript + breadcrumbs     │    │
                         │   │   │    ├─ screen capture (+opt. encode)│    │
                         │   │   │    └─ dummy keys + proxy trust     │    │
                         │   │   └─ Software Under Test (SUT)         │    │
                         │   └───────────────────────────────────────┘   │
                         └──────────────────────────────────────────────┘
```

### 4.1 Trust boundary

- **Host = trusted.** Holds real GitHub tokens, inference API keys, signing
  material, the MITM CA private key, and the artifact store.
- **Guest = untrusted.** Receives only **dummy** credentials. It trusts the
  MITM proxy's self-signed CA (installed at image-build time) so the host can
  transparently intercept TLS and substitute real keys.
- All guest→outside traffic is forced through the host MITM proxy and/or the
  Broker. There is **no direct egress** from guests except via host-mediated,
  pinned routes (enforced at the Hyper-V virtual switch / host firewall layer).

### 4.2 Host components (Rust crates / binaries)

- `qwanban-core` — domain types, job/case state machine, config.
- `qwanban-hyperv` — Hyper-V driver (VM create/clone/start/stop/checkpoint/destroy).
- `qwanban-broker` — host-side service guests call for mediated operations.
- `qwanban-proxy` — MITM HTTPS proxy (key rewrite, host pinning, audit).
- `qwanban-inference` — router to LM Studio / cloud providers.
- `qwanban-artifacts` — recording/clip/log storage + compression.
- `qwanban-web` — report viewer server.
- `qwanban-cli` — command-line entrypoint.
- `qwanban-guest` — the **qwan agent** (in-VM companion), cross-compiled for
  Windows + Linux, hosting the **qwan MCP** server.

### 4.3 Guest components (inside each untrusted VM)

- **Cline agent** — the actual tester/fixer. Runs as the **Cline SDK**, **Cline
  CLI**, or **Cline extension** (host-mediated inference via the Broker's
  OpenAI-compatible endpoint and a dummy key). This is the only "smart" process.
- **qwan agent** (`qwanban-guest`) — qwanban's companion. It:
  - emits **breadcrumbs** into the transcript and **exfiltrates** the transcript
    to the host,
  - controls **screen recording** (capture, and optionally compression locally),
  - performs **handoffs** (intervention / OS migration requests) to the host,
  - hosts the **qwan MCP** server.
- **qwan MCP** — an MCP server the Cline agent connects to, exposing:
  - **computer control** of the guest (mouse, keyboard, screenshot, window/app
    launch — the tools the Cline agent uses to *do* the QA),
  - `breadcrumb(label, kind)`, `clip(from, to, label)`,
  - `request_intervention(reason)`, `request_os_migration(target_os)`.

The qwan agent is updated **per case via file push** (see §5.7) so we can iterate
on qwan quickly without rebuilding base images.


## 5. Hyper-V VM Lifecycle

### 5.1 Provisioning

**Base images are just files the maintainer points qwanban at.** The maintainer
produces VHD/VHDX disk images (any way they like) and registers them in config
by **file path** (plus metadata: OS, label, default resource caps). There is no
qwanban-owned image build pipeline; qwanban only consumes the files.

```toml
# qwanban.toml (host config) — base image registry
[[images]]
name    = "sut-linux"
os      = "linux"
vhd     = "D:/qwanban/images/sut-linux.vhdx"   # maintainer-supplied file
default = true

[[images]]
name = "sut-windows"
os   = "windows"
vhd  = "D:/qwanban/images/sut-windows.vhdx"
```

qwanban drives Hyper-V through one of:

- **Hyper-V WMI v2 provider** (`root\virtualization\v2`) via Windows COM/WMI
  bindings (preferred for fine-grained control), or
- **PowerShell Hyper-V module** (`New-VM`, `New-VHD`, `Checkpoint-VM`, etc.)
  invoked as a fallback / bootstrap.

To keep provisioning fast and ephemeral:

- The maintainer's VHDX is treated as a read-only **parent**; each case gets a
  **differencing disk (AVHDX)** so clones are near-instant and cheap to discard.
- Optionally use **Hyper-V checkpoints** of a "booted + qwan agent ready" state
  to skip cold boot.

### 5.2 Networking

- A dedicated **internal/private vSwitch** connects guests to the host Broker
  and MITM proxy only.
- Host firewall + proxy enforce: guests can reach (a) the Broker endpoint and
  (b) the MITM proxy; all other egress is denied or forced through the proxy's
  pinned allowlist.

### 5.3 Host↔guest control channel

Primary control uses the network channel (Broker over the private vSwitch).
The bootstrap data plane (push/launch) also runs over **plain TCP on the
private vSwitch** — the same network the Broker uses. No Hyper-V sockets
(AF_HYPERV) are used; that mechanism requires host admin elevation, which
conflicts with the sandboxing goal. The TCP bootstrap channel is used to:

- push/update the **qwan agent** (see §5.7) and inject the job manifest,
- start the qwan agent + the Cline agent,
- stream transcript/breadcrumbs out,
- deliver stop/intervention signals.

> **Elevation note:** VM **creation and destruction** (Hyper-V management APIs —
> `New-VM`, `Remove-VM`, differencing disk creation, checkpoint/restore) still
> require an **elevated** (admin) host process. The **TCP bootstrap data plane**
> (push agent, write files, launch) does **not** — it is ordinary TCP over the
> private vSwitch and needs no special privileges on the host.

### 5.4 Prompt injection & job handoff

A **job manifest** (JSON/TOML — qwanban's own internal format, *not* the QA
script format) is delivered to the qwan agent containing:

- the task type (scripted QA / bug repro+fix),
- a pointer to the **QA script(s)** (plain text/Markdown, see §9.1) or the bug
  report text,
- repo coordinates + git ref,
- dummy credentials + proxy CA fingerprint,
- inference endpoint + allowed model list,
- resource caps and timeouts (also enforced host-side, see §5.8).

The qwan agent composes the system/task prompt for the **Cline agent** (handing
it the QA script / bug report as the task, and pointing it at the **qwan MCP**
for computer control), then starts it. Bisection is *not* a manifest field — it
is expressed to the Cline agent **at the prompt level** (see §9.3).

### 5.5 Teardown & human intervention

- **Default:** when a case completes (pass/fail/error), its differencing disk
  and VM are destroyed; artifacts are already streamed to the host store.
- **Human intervention:** if the agent calls `request_intervention(reason)`,
  the orchestrator marks the case **HELD**, takes a checkpoint, and does **not**
  auto-destroy. The maintainer can connect (VMConnect/RDP/SSH) and later resume,
  re-task, or discard.

### 5.6 OS migration (Linux ⇄ Windows)

Virtualization defaults to a **Linux** guest. The agent may call
`request_os_migration(target_os)` when it determines the case needs the other
OS (e.g. a Windows-only repro). On migration:

1. The Broker snapshots **portable case state** (repo working tree, transcript,
   breadcrumb index, accumulated artifacts, job manifest).
2. A new case VM is provisioned from the target-OS base image.
3. Portable state is rehydrated; the recording continues as a new segment
   stitched into the same job timeline.
4. The original VM is held briefly then discarded (or held if intervention).

State that is **not** portable (installed OS packages, OS-specific paths) is
re-derived from the target base image's pre-config.

### 5.7 Fast qwan-agent iteration (push, don't rebuild)

We will be revving the **qwan agent** (and qwan MCP) frequently. Rebuilding base
images for every change is too slow, so the qwan agent is **deployed per case at
runtime**, not baked into the image:

1. The base image contains only the **`qwan-stub` loader** (a tiny, stable
   TCP listener) plus the proxy CA trust. **No SSH** — the stub speaks a
   bootstrap protocol over plain TCP. See the
   [stub-loader component doc](components/stub-loader.md).
2. At case start, the host connects to `qwan-stub` over TCP and **pushes the
   current qwan agent build** (a self-contained binary + the qwan MCP) plus the
   manifest/token/CA, then **launches** it — all over the same TCP channel on
   the private vSwitch.
3. Because the agent is a single static Rust binary per OS/arch, the push is
   small and fast; iterating qwan is just "rebuild binary → next case picks it
   up." No image rebuild required. (The stub is stable and changes ~never; on the
   rare breaking stub change the image is rebuilt.)

The image only needs rebuilding when the **SUT** or OS-level prerequisites
change — which is the maintainer's concern, not qwan's.

> Note: this keeps the *expensive* thing (the configured SUT image) stable while
> the *frequently-changing* thing (qwan) is hot-swapped each run.

### 5.8 Resource caps (host-enforced)

Resource caps are configured **on the host** and enforced at the Hyper-V layer
(not trusted to the guest). Per-image defaults can be overridden per-job:

```toml
[defaults.resources]
vcpu          = 4
memory_mb     = 8192
disk_gb       = 64
max_runtime   = "45m"     # wall-clock cap per case
max_concurrent_cases = 3  # hard host-wide cap
```

**Admission: hard cap, reject immediately — no queue.** When a job is submitted
and all `max_concurrent_cases` slots are in use, the submission **errors out**
(`ResourceExhausted`); qwanban does **not** queue or wait. The caller decides
whether to retry later. This keeps v1 trivial; a queue/bin-packing policy can be
added later behind the same `submit` API. Per-case caps map to Hyper-V VM
settings (processor count, memory, dynamic-memory limits) and a host-side
watchdog timer enforces `max_runtime`.


## 6. Screen Recording, Breadcrumbs & Clippings

### 6.1 Continuous recording

- The **qwan agent** continuously captures the guest display and streams it to
  the host (over the private vSwitch / TCP).
- **Compression can happen either in the guest or on the host — whichever is
  easier.** We don't treat the guest's media as security-sensitive (a malicious
  guest can already lie about anything it shows), so there is no hard requirement
  to encode host-side. Practical default: **compress in the guest** (cheaper
  transport) and have the host validate/repackage; switch to host-side encode
  only if a guest lacks a usable encoder.
- Output is stored as a compressed, **timestamped, segmented** stream (e.g.
  fragmented MP4 `H.264`/`AV1` or WebM) so segments can be addressed by time.

### 6.2 Breadcrumbs

- The Cline agent calls the **qwan MCP** `breadcrumb(label, kind)` tool; the
  **qwan agent** stamps and writes structured, monotonically-timestamped events
  into the transcript (e.g. `step.begin`, `click`, `assert.fail`,
  `repro.confirmed`, `fix.verified`). Each carries a wall-clock + recording
  offset so it maps to an exact frame.
- The transcript and recording share a **single guest-local monotonic clock**
  (the guest authors both, so this is bookkeeping, not synchronization — see
  components README §S2) so playback can scrub the video from any transcript line
  and vice versa.

### 6.3 Clippings

- The Cline agent calls the **qwan MCP** `clip(from_breadcrumb, to_breadcrumb,
  label)` tool.
- The host cuts the segment from the stored recording (no re-encode where
  possible, via keyframe-aligned segment boundaries), labels it, and attaches it
  to the report / PR as evidence.

### 6.4 Web report viewer (`qwanban-web`)

- Serves a per-job page: synchronized **transcript ↔ video** scrubber,
  breadcrumb timeline, attached clippings, logs, and repro steps.
- Read-only, served from the host artifact store; suitable for linking from PRs.

## 7. Inference Routing

- Guests have **no GPU**. They make inference requests to the **Broker's
  inference endpoint** (presented to the guest as an OpenAI-compatible API with
  a **dummy** key).
- `qwanban-inference` routes to:
  - the host **LM Studio** server (fixed model set, OpenAI-compatible API), or
  - a **cloud** provider, with the real key injected on the host side.
- The guest's allowed model list is constrained by the job manifest; requests
  for disallowed models are rejected at the Broker.
- Because inference may also be requested by the *agent's own tooling* over
  HTTPS, those requests can alternatively traverse the **MITM proxy** path
  (Section 8), which performs the same dummy→real key rewrite. The Broker path
  is preferred for first-party inference; the proxy path covers third-party
  tools that hardcode public endpoints.

## 8. MITM HTTPS Proxy & Key Rewriting

### 8.1 Purpose

Guests never hold real secrets. Instead they hold **dummy keys** and trust a
host **self-signed CA**. All HTTPS egress is intercepted by the host MITM proxy
which:

1. **Pins** requests to an allowlist of trusted hosts (e.g.
   `api.github.com`, `api.openai.com`, the inference provider). Anything else is
   blocked.
2. **Swaps** any known dummy string for its real secret via a **global
   search→replace** table held in the host `secrets.toml`. Dummies are real-looking
   unique strings (not a sentinel), so the agent can juggle multiple tokens and
   even hide them in a chroot; the proxy just finds the dummy bytes (in headers,
   URL, or body) and replaces them with the real secret bytes. **No header-format
   logic, no auto-injection** — a request to an allowlisted host carrying no known
   dummy is forwarded verbatim.
3. **Audits** every mediated request (destination, method, path, bytes, which
   real key was used) for later abuse review.

Future extensions at this layer: **rate limits, quota, anomaly/abuse detection,
request scrubbing.**

### 8.2 How the guest gets MITM'd

- At base-image build time the maintainer installs the proxy's CA cert into the
  guest trust store (Windows cert store / Linux CA bundle) and sets system proxy
  env (`HTTPS_PROXY`, etc.).
- The guest therefore transparently trusts the host's intercepting cert; the
  proxy terminates TLS, inspects/rewrites, and re-originates the request to the
  real upstream over a properly validated TLS connection.

### 8.3 Implementation choice (Rust, not Python)

`mitmproxy` is the obvious reference, but it is Python. We want a **Rust**
implementation for a single-language host, easier embedding, and performance.
Evaluated options:

| Crate | Notes |
|-------|-------|
| **`hudsucker`** | Purpose-built **MITM HTTP/S proxy** library; modify requests/responses/WebSocket messages; HTTP/2 support; built-in CA via **`rcgen`** (`RcgenAuthority`) or OpenSSL (`OpensslAuthority`); rustls/native-tls clients. **Recommended primary.** |
| `http-mitm-proxy` | Lower-level MITM proxy library (Burp-style backend); viable alternative. |
| `rama` | General async networking/proxy framework; more building-blocks, more work. |
| `rcgen` | X.509 cert generation — used for the CA + leaf certs regardless of proxy lib. |

**Decision:** build `qwanban-proxy` on **`hudsucker`** with an `rcgen`-generated
CA. Hudsucker's request/response interception hooks implement host pinning and
the dummy→real key rewrite; the CA private key stays on the host only. Revisit
`rama` only if we outgrow hudsucker's extension points.


## 9. Use Case 1 — Scripted QA

### 9.1 QA script format (human-readable, not YAML)

QA scripts are **plain human-readable text / Markdown** — the kind of thing a
human QA tester would write, e.g. `qa/checkout-flow.md`:

```markdown
# Checkout flow

Repo: org/app @ main

1. Launch the app.
2. Add an item to the cart.
3. Enter the promo code `SAVE10` in the promo field.
4. Confirm the order total shows **$9.00** (this is the SAVE10-discounted price).
5. If the total is wrong, that's a bug — capture a repro.
```

There is **no rigid step schema**. The **Cline agent** reads the script and
carries it out using **Anthropic's computer-use tool** (screenshot, click, type,
key, scroll — executed by the qwan computer-use backend, see §17 / sequences
§7.4), falling back to vision/LLM judgement for brittle UI. It emits a
**breadcrumb** (via the qwan MCP) as it begins/ends each step and on any
assertion outcome.

Rationale: the agent is capable of interpreting natural-language intent, so a
declarative selector DSL adds brittleness with little benefit. Free-text scripts
are also far easier for the maintainer to author and version alongside docs.

### 9.2 Run & report

- The Cline agent works through the script; on a failure it cuts a clipping
  (`step.begin`→`assert.fail`) via the qwan MCP and records logs + environment
  metadata.
- The **QA report** aggregates pass/fail per step/script with linked repros and
  the synchronized video.

### 9.3 Bisection — handled at the prompt level

Bisection is **not** a separate qwanban subsystem, CLI verb, or host-driven
binary search. Instead, when commit attribution is desired, it is expressed to
the **Cline agent in the prompt** (e.g. "this QA flow passes at `v1.2.0` but
fails at `main`; reproduce the failure, then narrow down which commit introduced
it"). The agent uses ordinary git tooling inside the VM (e.g. `git bisect`,
checkouts, rebuilds) and computer control to classify each candidate, and emits
breadcrumbs / a clip for the culprit commit.

This keeps qwanban simpler (no deterministic-replay harness to maintain) and
lets the agent apply judgement to flaky GUI steps. The host's only role is the
usual one: provide the VM, mediate git/inference, capture the recording.

## 10. Use Case 2 — Bug Repro & Fix

1. **Ingest** a bug report (manual input, file, or issue-tracker/GitHub source
   via the host-side Git/PR proxy).
2. Provision a Linux case (default) from the SUT base image; the qwan agent
   hands the report to the Cline agent as the task.
3. The Cline agent attempts to **reproduce**; on success it emits
   `repro.confirmed` (via qwan MCP) and cuts a repro clip. (It may
   `request_os_migration` if the bug is OS-specific.)
4. The Cline agent develops a **fix**, re-runs the repro to verify
   (`fix.verified` + verification clip).
5. The Cline agent opens a **PR** via the host Git/PR proxy (real GitHub token is
   injected host-side; the guest only ever used a dummy token). The PR body
   embeds links to the repro/fix clippings and the report page.
6. If blocked, the Cline agent calls the qwan MCP `request_intervention`; the VM
   is held.

## 11. CLI & Rust API

### 11.1 CLI sketch

```
# Run a QA script (plain text/Markdown). Commit attribution, if wanted, is just
# phrased inside the script or via --note, not a separate verb.
qwanban job run --script ./qa/checkout-flow.md --base sut-linux --ref main
qwanban job run --script ./qa/checkout-flow.md --base sut-linux \
    --note "passes at v1.2.0, fails at main — find the bad commit"
qwanban job fix --bug ./reports/1234.md --base sut-linux
qwanban job list
qwanban job hold <job-id>          # force human intervention / preserve VM
qwanban job resume <job-id>
qwanban job destroy <job-id>
qwanban images list                # base images registered by file path
qwanban report open <job-id>       # opens web report
qwanban proxy status               # MITM allowlist + audit summary
```

### 11.2 Rust API sketch

```rust
let engine = Qwanban::from_config(cfg)?;

// `script` is the contents (or path) of a human-readable text/Markdown file.
let job = engine.submit(JobSpec::ScriptedQa {
    base_image: "sut-linux".into(),
    scripts: vec![QaScript::from_path("qa/checkout-flow.md")?],
    git_ref: "main".into(),
    // Bisection / commit-attribution is just extra natural-language guidance:
    note: Some("passes at v1.2.0, fails at main — find the bad commit".into()),
}).await?;

let outcome = job.await_completion().await?;     // streams breadcrumbs/events
for repro in outcome.repros() { /* clip paths, transcript spans */ }

let fix = engine.submit(JobSpec::BugFix {
    base_image: "sut-linux".into(),
    report: BugReport::from_path("reports/1234.md")?,
}).await?;
let pr_url = fix.await_completion().await?.pr_url();
```

Both CLI and API are thin layers over `qwanban-core`'s job state machine. Note
there is **no** `Bisect` job type — it is folded into the QA/fix prompt.


## 12. Job/Case State Machine

```
submit → (Rejected: ResourceExhausted if no free slot — §5.8, no queue)
       → Admitted → Provisioning → Booting → QwanAgentPushed → ClineAgentReady → Running
   Running → (Completed | Failed | Error)             → Teardown → Archived
   Running → InterventionRequested → Held  (no auto-teardown)
   Running → OsMigration → Provisioning(new OS) → Running
   Held → (Resumed → Running | Discarded → Teardown)
```

There is **no `Queued` state** — admission is a synchronous accept/reject at
`submit` against the hard `max_concurrent_cases` cap.

(`QwanAgentPushed` is the per-case file push of §5.7; `ClineAgentReady` is once
the qwan agent has launched the Cline agent and the qwan MCP is connected.)

- All transitions are host-driven and logged.
- Artifacts are streamed continuously, so even `Error`/crash leaves a usable
  partial report.

## 13. Security Model

- **Secrets stay on host.** Real GitHub tokens, inference keys, CA private key,
  and signing material never enter a guest. Guests get dummy stand-ins.
- **Forced mediation.** Network policy (private vSwitch + host firewall) ensures
  guests cannot bypass the Broker/MITM proxy.
- **Host pinning.** Proxy allowlist constrains outbound destinations; unknown
  hosts are blocked and audited.
- **Exfiltration is explicitly out of scope (for now).** The keys the guest
  holds are **fake**, so we **don't care if the guest exfiltrates them** — they
  are worthless off-host. We also don't currently police what the guest sends to
  *allowed* hosts. The real keys never leave the host; that is the only property
  we rely on. (Much later we may bolt a **Bayesian classifier** / anomaly
  detector onto the proxy/Broker to flag abusive traffic, but it is not a v1
  requirement.)
- **Media is not security-sensitive.** Recordings come from an untrusted guest
  and may be compressed there; we treat them as *evidence the agent chose to
  show*, not as trusted attestation. Nothing security-critical depends on them.
- **Ephemerality.** Default destroy-on-done limits persistence of any guest
  compromise; held VMs are explicitly opt-in.
- **Resource caps** (host-enforced, §5.8) bound the blast radius of a runaway or
  hostile guest (CPU/RAM/disk/runtime, plus a host-wide concurrency cap).
- **Auditing.** Every mediated request and key substitution is logged — useful
  for later rate-limit / abuse-detection features even though exfil isn't a
  current concern.

## 14. Technology Stack

| Concern | Choice |
|---------|--------|
| Language (host + qwan agent) | Rust (async, Tokio) |
| In-guest tester | **Cline** (Cline SDK / CLI / extension) |
| In-guest companion | **qwan agent** (`qwanban-guest`) + **qwan MCP** (computer control, breadcrumbs, clips, handoffs) |
| Hypervisor | Hyper-V (WMI v2 / PowerShell module) |
| Host↔guest bootstrap | plain TCP over the private vSwitch (no Hyper-V sockets) |
| qwan agent delivery | per-case push over **TCP** via the baked-in `qwan-stub` loader (no SSH) — no image rebuild |
| Broker transport | gRPC or HTTP/2 over private vSwitch |
| MITM proxy | `hudsucker` + `rcgen` CA |
| Inference | LM Studio (OpenAI-compatible) + cloud, via `qwanban-inference` |
| Recording | guest capture, compress in guest *or* host (whichever is easier); fragmented MP4/WebM, H.264/AV1 |
| Web report | `qwanban-web` (axum or similar) + JS player |
| Storage | local artifact store (content-addressed), compressed |
| Disks | maintainer-supplied VHD/VHDX (read-only parent) + per-case differencing AVHDX |
| Secret vault | a **plain file on the host filesystem** (e.g. `secrets.toml`), kept simple/convenient; harden later if needed |
| Resource caps | host config, enforced via Hyper-V VM settings + watchdog |

## 15. Open Questions

Several earlier questions are now **decided**: in-guest tester = **Cline**;
companion = **qwan agent + qwan MCP**; bisection = **prompt-level**; base images
= **maintainer file paths**; secret vault = **host file**; exfil = **not a
concern**; resource caps = **host config**. Remaining:

1. ~~**qwan MCP computer-control surface.**~~ **DECIDED:** Cline is patched to use
   **Anthropic's built-in computer-use tool** (beta `computer-use-2025-01-24`,
   tool `computer_20250124`) with Anthropic's recommended resolutions/scaling.
   Computer control therefore runs through a qwan **computer-use executor**
   (`input-injection.md`), *not* the qwan MCP. The qwan MCP carries only
   qwan-specific tools (breadcrumb/clip/handoff/finish). Per-OS injection:
   Windows `SendInput`, Linux `uinput` (X11/Wayland) with XTEST fallback. See
   `sequences.md` §7.4 and `components/input-injection.md`.
2. ~~**Cline integration shape.**~~ **DECIDED:** qwanban is **agent-form-factor
   agnostic** — it drops a maintainer-specified set of **files** into the guest
   and runs a **launch command** (`pwsh`/`zsh`/`bash`/…), with task + creds +
   `QWAN_MCP_ADDR`/`QWAN_CUXEC_ADDR` injected via env. Whatever that command
   starts (patched Cline CLI, SDK harness, script) is the maintainer's choice.
   See `components/agent-lifecycle.md` (`manifest.agent`).
3. ~~**qwan agent push mechanics.**~~ **DECIDED:** a **TCP stub loader**
   (`qwan-stub`) baked into every base image — **no SSH** (avoids OpenSSH setup
   on Windows). The host pushes the agent + files and launches over **plain TCP
   on the private vSwitch**. See `components/stub-loader.md`.
4. ~~**Windows guest input automation (UIA vs. vision).**~~ **DECIDED: vision.**
   qwanban provides only screenshots + coordinate input (the Anthropic
   computer-use surface). There is **no host-side accessibility/automation-tree
   integration** (no UIA/AT-SPI plumbing in qwan). If the agent wants an
   accessibility/automation tree, it can derive that **itself, on-device** (its
   own tooling inside the guest). Keeps qwan OS-agnostic and avoids per-OS a11y
   maintenance. See `components/input-injection.md`.
5. ~~**Concurrency scheduling.**~~ **DECIDED: hard cap, reject immediately, no
   queue.** `submit` accepts only if a `max_concurrent_cases` slot is free;
   otherwise it returns `ResourceExhausted` and the caller retries later. No
   queue, no bin-packing, no priority in v1 (all can be added later behind the
   same `submit` API). See §5.8.
6. ~~**Secret-file format & hot-reload.**~~ **DECIDED:** `secrets.toml` holds `[real]`
   secret values by name + a `[[rewrite]]` table mapping each **unique, real-looking
   dummy string** (search) → a real secret name (replace). The proxy and
   inference-router apply a **global search→replace** (no per-host header-format
   logic, no auto-injection; an allowed request with no known dummy passes through).
   Dummies are distinct & secret-shaped so the agent can juggle multiple tokens.
   **Hot-reloaded** via file watch + atomic snapshot swap (no restart). Plain text
   on the host fs for now (harden later). See `components/mitm-proxy.md`.

## 16. Milestones (suggested)

- **M0 — Skeleton:** `qwanban-core` + CLI + Hyper-V clone/start/stop/destroy
  from maintainer-supplied VHDX paths; host resource caps.
- **M1 — Channel + push:** TCP bootstrap, **per-case qwan agent push**
  (§5.7), prompt injection, transcript streaming.
- **M2 — qwan agent + qwan MCP:** breadcrumbs, handoffs, and **computer-control**
  MCP tools; wire up the **Cline agent** to run a task using the MCP.
- **M3 — Proxy:** `hudsucker` MITM with pinning + dummy→real key rewrite (keys
  from the host `secrets.toml`) + audit.
- **M4 — Inference:** Broker inference endpoint → LM Studio/cloud.
- **M5 — Recording:** guest capture (compress in guest or host, whichever is
  easier) + breadcrumb sync + clippings.
- **M6 — Web report:** synchronized transcript/video viewer.
- **M7 — Use case 1:** scripted QA from text/Markdown scripts + report
  (prompt-level bisection included, no separate subsystem).
- **M8 — Use case 2:** bug repro/fix + PR via Git proxy.
- **M9 — OS migration + human-intervention hold.**

## 17. Detailed Design (sequences + component docs)

This document is the **architecture**. The next level of detail lives in:

- [`sequences.md`](sequences.md) — **Section 7**: UML-style sequence diagrams for
  the common interactions (provisioning, agent push, registration, inference,
  computer control, breadcrumbs, video, clipping, PR, web report, intervention,
  OS migration, teardown). Steps are numbered `7.<scenario>.<step>`.
- [`components/`](components/README.md) — per-component sub-design docs, each
  detailed enough to hand to an implementer, with **shared contracts**
  (identifiers, the timeline/clock model, transport, auth, error model,
  versioning) that make the pieces interlock end-to-end. The component README
  carries the cross-component interface ownership table and the dependency-aware
  build order.

Each component doc declares the exact `7.x.y` steps it implements ("Sequence
coverage"), so the diagrams and the docs stay in lockstep.

See also [`dev-workflow.md`](dev-workflow.md) for the development constraints
(we build **inside a VM**, so host-touching code is trait-mocked + integration-
gated and everything else is unit-tested anywhere) and the **git worktree +
merge** workflow used to implement components in parallel.


