# qwanban — Component Sub-Design Docs

These docs decompose [`../design.md`](../design.md) and
[`../sequences.md`](../sequences.md) into implementable units. Each doc is
written so it can be handed to a sub-agent (or engineer) to implement
independently, while the **shared contracts** below guarantee the pieces fit
together end-to-end.

## Component index

| Doc | Owns | Crate/artifact |
|-----|------|----------------|
| [`broker-protocol.md`](broker-protocol.md) | gRPC control plane, streams, framing, auth | `qwanban-broker` + generated stubs |
| [`hyperv-driver.md`](hyperv-driver.md) | VM lifecycle, disks, networking, TCP, checkpoints | `qwanban-hyperv` |
| [`stub-loader.md`](stub-loader.md) | TCP bootstrap loader baked into images (push/write/launch) | `qwan-stub` (baked) |
| [`agent-lifecycle.md`](agent-lifecycle.md) | per-case push, file injection, launch, register, handoffs, teardown | `qwanban-guest` + `qwanban-core` |
| [`mcp-server.md`](mcp-server.md) | qwan MCP tool surface exposed to Cline | `qwanban-guest` (mcp module) |
| [`input-injection.md`](input-injection.md) | OS-level mouse/keyboard/window control | `qwanban-guest` (input module) |
| [`video-capture-encode.md`](video-capture-encode.md) | capture, encode, segment, upload, screenshot pull | `qwanban-guest` (capture) + `qwanban-broker` ingest |
| [`breadcrumbs-transcript.md`](breadcrumbs-transcript.md) | breadcrumb model, transcript stream, durability | `qwanban-guest` + `qwanban-broker` |
| [`mitm-proxy.md`](mitm-proxy.md) | TLS MITM, host pinning, key rewrite, audit, git/PR proxy | `qwanban-proxy` |
| [`inference-router.md`](inference-router.md) | OpenAI-compatible endpoint, model allowlist, routing | `qwanban-inference` |
| [`artifact-store-and-clipping.md`](artifact-store-and-clipping.md) | storage layout, indices, clip cutting, web report serving | `qwanban-artifacts` + `qwanban-web` |

---

## Shared contracts (NORMATIVE — every doc must conform)

### S1. Identifiers

All IDs are URL-safe strings. Format: `<prefix>_<ulid>` (ULID = sortable,
time-ordered).

| ID | Prefix | Scope | Allocated by |
|----|--------|-------|--------------|
| `job_id` | `job_` | global | `orch` (7.1.2) |
| `case_id` | `case_` | global; belongs to one job | `orch` (7.1.3) |
| `breadcrumb_id` | `bc_` | **case-monotonic integer** `1..N` (NOT ulid) | `guest` (7.5.3) |
| `clip_id` | `clip_` | per case | `guest` (7.7.1, client-generated, idempotent) |
| `segment_idx` | integer `0..N` | per case, contiguous | `guest` capture (7.6.3) |
| `seq` | integer `0..N` | per case transcript stream | `guest` (7.5.6) |
| `event_id` | `evt_` | per case input events | `guest`/`mcp` (7.4.8) |

A job spanning OS migration (7.11) keeps **one `job_id`** but uses a **new
`case_id`** per VM. Breadcrumb/segment/seq counters are **continuous across
cases of the same job** (the broker maintains the running counter so the job
timeline is unbroken).

### S2. Case timeline (bookkeeping, NOT clock synchronization)

There is **no clock synchronization** between guest and host. The guest authors
*both* the video stream and the transcript, so correlating a breadcrumb to a
video position is a **bookkeeping** problem local to the guest, not a
cross-machine sync problem.

- Each case has a single **guest-local monotonic timebase** the qwan agent
  establishes when the capture pipeline starts: `t0 = monotonic_now()` at the
  first captured frame. This `t0` is the **case timeline origin** and never
  changes for the life of the case.
- **`timeline_ns` = `monotonic_now() - t0`** (nanoseconds since the first frame),
  measured on the *guest's own* monotonic clock. Every breadcrumb, video fragment
  boundary, input event, and clip boundary is stamped with `timeline_ns` taken
  from this one clock.
- Because video frames and breadcrumbs are stamped from the **same** guest clock,
  a breadcrumb's `timeline_ns` indexes directly into the video — exact by
  construction, with no skew/drift/round-trip estimation.
- The host **stores `timeline_ns` verbatim** and never reinterprets or "corrects"
  it. The broker does not send clock anchors, skew, or `clock_sync`.
- Wall-clock time (RFC3339) MAY be attached as **display metadata** only; it is
  never a join key.
- **OS migration (job spanning multiple cases):** each case has its own `t0` and
  its own `timeline_ns` starting at 0. They are stitched into one job timeline by
  the host **at presentation time** using a per-case `timeline_offset_ns`
  (cumulative duration of prior cases), assigned by the broker when it opens the
  successor case. The guest still only ever deals with its own case-local
  `timeline_ns`; concatenation is a host-side report concern.
- **Join rule:** anything correlated across video/transcript/clips MUST use
  case-local `timeline_ns`.

### S3. Transport

- **Control plane:** gRPC (tonic) over the private vSwitch, TLS using the
  broker's server cert; guest authenticates every RPC with the **`case_token`**
  (S4) in metadata `x-qwan-case-token`. Bootstrap (push/launch) uses **plain
  TCP** on the same private vSwitch (see hyperv-driver + agent-lifecycle).
- **Bulk streams** (video segments, transcript batches) are client-streaming
  gRPC with application-level **acks + resume offsets** (at-least-once;
  receivers dedupe by `(case_id, segment_idx)` / `(case_id, seq)`).
- **Inference & proxied HTTPS** are ordinary HTTP(S), not gRPC.
- **MCP** is local to the guest (Cline ↔ qwan MCP) over loopback; it never
  crosses the host boundary.

### S4. AuthZ

- `case_token`: per-case bearer secret minted by broker at `open_case` (7.1.5),
  delivered into the VM as a file (7.1.15), presented on every guest→broker RPC
  and on the inference endpoint as the OpenAI `api_key` (the literal `DUMMY`
  string maps to the case via a separate header `x-qwan-case-id`, OR the dummy
  key *is* the case token — see inference-router for the chosen binding).
- `case_token` is **invalidated** at `close_case` (7.12.10). All guest creds are
  worthless off-host (fake keys; exfil not a concern per §13).

### S5. Error model

All RPC errors use a common envelope:

```
QwanError { code: enum, message: string, retryable: bool, details: map }
```

`code` ∈ { `InvalidArg`, `Unauthenticated`, `NotFound`, `FailedPrecondition`,
`ResourceExhausted`, `Unavailable`, `Internal`, `Timeout`, `Blocked` (proxy
pin), `NotAllowed` (model/policy) }. `retryable=true` ⇒ caller may retry with
backoff; streams resume from last acked offset.

### S6. Versioning

- A single `PROTOCOL_VERSION` (semver) is exchanged at `Register` (guest sends
  its `agent_version`; broker sends `protocol_version`). Mismatch in **major** ⇒
  broker rejects with `FailedPrecondition`; orchestrator re-pushes a matching
  qwan-guest binary (cheap per §5.7). Minor/patch are backward-compatible.
- Because the qwan agent is pushed per case (7.1.13), host and guest builds are
  always from the same release train in practice; the handshake guards manual
  drift.

### S7. Configuration & secrets

- Host config: `qwanban.toml` (image registry, resource caps, proxy allowlist,
  inference routes). Defined across design.md §5.1, §5.8, §7, §8.
- Secrets: `secrets.toml` (host file) — `[real]` values by name + a `[[rewrite]]`
  dummy→secret search→replace table. Hot-reloaded (Q6). Only `vault` and the
  proxy/inference-router read it. Never serialized into any guest-bound payload
  (the guest gets the *dummy* strings, which are worthless off-host).

### S8. Definition of "done" for a component

A component doc's implementation is complete when:
1. it implements every `7.x.y` step listed in its "Sequence coverage" section;
2. it conforms to S1–S7;
3. its public interface (the "Interfaces" section) matches what dependent docs
   import — verified by the cross-doc interface table below;
4. it ships the tests named in its "Testing" section.

---

## Cross-component interface ownership

To avoid two docs defining the same wire type differently, each shared type has
**one owning doc**. Others import it by name and MUST NOT redefine fields.

| Shared type | Owner doc |
|-------------|-----------|
| `OpenCase`, `Register`, `Heartbeat`, `Handoff`, `CaseResult`, gRPC services | broker-protocol |
| `TranscriptEntry`, `BreadcrumbIn`, `Breadcrumb` | breadcrumbs-transcript |
| `VideoSegment`, segment index, `RawFrames` | video-capture-encode |
| `ClipRequest`, clip asset, storage layout | artifact-store-and-clipping |
| `InputEvent`, `InputAck`, `ComputerAction`, computer-use action mapping, coordinate scaling | input-injection (the computer-use backend) |
| MCP tool schemas (qwan-only: `breadcrumb`,`clip`,`request_intervention`,`request_os_migration`,`finish`) | mcp-server |
| Cline agent-loop adapter (drives Anthropic computer-use → `cuxec`) | agent-lifecycle |
| `JobSpec`, `JobOutcome`, manifest, case state machine | agent-lifecycle (+ qwanban-core) |
| VM handle, disk/network ops, TCP | hyperv-driver |
| inference route config, model allowlist | inference-router |
| proxy allowlist, key-rewrite rules, audit record | mitm-proxy |

## Build order (dependency-aware)

1. **broker-protocol** (everything depends on the wire types).
2. **hyperv-driver**, **stub-loader**, **agent-lifecycle** (parallel after 1;
   stub-loader + agent-lifecycle share the bootstrap protocol framing).
3. **input-injection** (the computer-use backend), **video-capture-encode**,
   **breadcrumbs-transcript**, **mcp-server** (parallel; depend on 1).
   - The **Cline agent-loop adapter** (in agent-lifecycle) wires the patched
     Cline's Anthropic computer-use tool to `input-injection`'s
     `ComputerUseExecutor`; computer control does **not** go through mcp-server.
4. **mitm-proxy**, **inference-router** (parallel; depend on 1 + vault).
5. **artifact-store-and-clipping** + web (depends on video + transcript +
   clip contracts).

Each doc lists its concrete upstream/downstream dependencies in its "Dependencies"
section.

