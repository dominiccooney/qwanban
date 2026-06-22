# Component: Broker Protocol (`qwanban-broker`)

> Owns the wire contract between guest and host. **Read
> [`README.md`](README.md) §S1–S8 first** — this doc defines the messages those
> contracts reference. Crate: `qwanban-broker` (host) + `qwanban-proto`
> (shared `.proto` + generated tonic stubs used by both host and `qwanban-guest`).

## Purpose & scope

The Broker is the single host-side endpoint every untrusted guest talks to for
mediated operations: registration, heartbeats, transcript ingest, video ingest,
clip requests, handoffs (intervention / OS migration), and final results. It is
**not** the inference endpoint (that's `qwanban-inference`, a sibling HTTP
service that shares the vault and case registry) and **not** the MITM proxy.

The Broker is *control + ingest*. It is intentionally dumb about media/transcript
*content* — it stores and indexes via `qwanban-artifacts`.

## Sequence coverage

Owns broker-side of: **7.1.4–7.1.5**, **7.2.2–7.2.3, 7.2.9–7.2.10**,
**7.3.A9 (usage ingest)**, **7.5.6–7.5.8**, **7.6.4–7.6.7**, **7.7.4–7.7.8**,
**7.10.3–7.10.4**, **7.11.3–7.11.9 (broker parts)**, **7.12.4–7.12.6, 7.12.10**.

## Dependencies

- Upstream: `qwanban-core` (case registry, state machine, ID allocation),
  `qwanban-artifacts` (storage), `vault` (case_token mint/verify).
- Downstream consumers of `qwanban-proto`: `qwanban-guest`, `qwanban-inference`
  (case lookup), `qwanban-core` (orch callbacks).

## Transport & framing

- **gRPC over TLS** (tonic + rustls), listening on the private vSwitch address
  (e.g. `https://10.0.75.1:7443`). Server cert is the broker's own cert; the
  guest pins it by SPKI fingerprint delivered in `manifest.json` (7.1.15).
- Bootstrap RPCs (push/launch) are NOT here — they go over plain TCP on the
  private vSwitch and are owned by hyperv-driver + agent-lifecycle.
- Every RPC carries metadata `x-qwan-case-token` (S4) and `x-qwan-case-id`.
  Interceptor verifies token↔case binding before the handler runs; failure ⇒
  `Unauthenticated`.
- Errors map `QwanError` (S5) onto gRPC `Status` (`code` → status code,
  envelope in `details`).

## Service definitions (`qwanban.proto`, v1)

```proto
syntax = "proto3";
package qwanban.v1;

service CaseControl {
  // Orchestrator-only (host-internal, mTLS or UDS): create/destroy case state.
  rpc OpenCase(OpenCaseReq) returns (OpenCaseResp);     // 7.1.4
  rpc CloseCase(CloseCaseReq) returns (CloseCaseResp);  // 7.12.10

  // Guest-facing:
  rpc Register(RegisterReq) returns (RegisterResp);     // 7.2.2
  rpc Heartbeat(stream HeartbeatReq) returns (stream HeartbeatResp); // 7.2.9 (bidi)
  rpc Handoff(HandoffReq) returns (HandoffResp);        // 7.10.3 / 7.11.3
  rpc ReportResult(CaseResultReq) returns (CaseResultResp); // 7.12.4
}

service Ingest {
  rpc AppendTranscript(stream TranscriptBatch) returns (stream AppendAck); // 7.5.6
  rpc UploadVideo(stream VideoChunk) returns (stream VideoAck);            // 7.6.4
  rpc RequestClip(ClipRequest) returns (ClipResponse);                     // 7.7.4
  rpc PushPortableState(stream StateChunk) returns (StateAck);             // 7.11.4
}
```

### Key messages

```proto
message OpenCaseReq {
  string case_id = 1; string job_id = 2;
  Manifest manifest = 3;            // owned by agent-lifecycle
  ResourceCaps caps = 4;            // owned by hyperv-driver/core
}
message OpenCaseResp { string case_token = 1; string broker_endpoint = 2;
                       bytes broker_cert_spki_sha256 = 3; }

message RegisterReq {
  string case_id = 1; string agent_version = 2;
  GuestInfo guest_info = 3;         // os, arch, screen_w, screen_h, dpi
}
message RegisterResp {
  // NOTE: no clock anchor/skew. The guest owns its case-local timeline (S2);
  // it stamps timeline_ns = monotonic_now() - t0 and the host stores it verbatim.
  int64 timeline_offset_ns = 1;     // 0 for first case; cumulative prior-case
                                    // duration for an OS-migration successor (presentation-only)
  repeated string allowed_models = 2;
  string protocol_version = 3;
  IngestLimits limits = 4;          // max segment bytes, batch size, hb interval
}

message HandoffReq {
  string case_id = 1;
  enum Kind { INTERVENTION = 0; OS_MIGRATION = 1; }
  Kind kind = 2; string reason = 3;
  string target_os = 4;             // for OS_MIGRATION
  string portable_state_ref = 5;    // set after PushPortableState completes
}

message CaseResultReq {
  string case_id = 1;
  enum Result { PASS=0; FAIL=1; FIXED=2; UNREPRODUCIBLE=3; ERROR=4; }
  Result result = 2; string summary = 3; string pr_url = 4;
  repeated string clip_ids = 5;
}
```

(`TranscriptBatch`/`TranscriptEntry` are owned by breadcrumbs-transcript;
`VideoChunk`/`VideoSegment` by video-capture-encode; `ClipRequest`/`ClipResponse`
by artifact-store-and-clipping. This doc references them; those docs define the
fields.)

## Streaming semantics (ingest)

Both `AppendTranscript` and `UploadVideo` are **client-streaming with periodic
acks** (modeled as bidi so the server can ack mid-stream and request resume):

- Guest sends items tagged with their offset (`seq` for transcript,
  `segment_idx` for video).
- Server persists then returns `AppendAck{ up_to_seq }` / `VideoAck{ up_to_idx }`.
- **At-least-once:** if the stream breaks, guest reopens and resumes from
  `up_to_* + 1`; server **dedupes** idempotently by offset (S3).
- **Backpressure:** server advertises `IngestLimits` (max in-flight bytes); guest
  spools to local disk if unacked backlog exceeds a threshold, never blocking the
  capture/transcript producers.

## State machine callbacks (broker → orch)

The broker translates guest signals into case-state transitions consumed by
`qwanban-core` (in-process or via an internal channel — not gRPC):

| Guest signal | Emitted transition | Sequence |
|--------------|--------------------|----------|
| `Register` ok | `QwanAgentPushed → ClineAgentReady` | 7.2.10 |
| first `Heartbeat(Running)` | `→ Running` | 7.2.10 |
| heartbeat timeout (> 3× interval) | `→ Error(guest_lost)` | — |
| `Handoff(INTERVENTION)` | `→ Held` (cancel teardown timer) | 7.10.4 |
| `Handoff(OS_MIGRATION)` | spawn sibling case, same job | 7.11.5 |
| `ReportResult` | `→ Completed/Failed/Error` | 7.12.6 |

## Heartbeat

- Bidi stream; guest sends `HeartbeatReq{ status, capture_health, queue_depths }`
  every `hb_interval` (default 5s, from `IngestLimits`).
- Server replies `HeartbeatResp{ directives[] }` where `directives` may include
  `Pause`, `Resume`, `Drain`, `Stop` (used during teardown 7.12 and intervention
  7.10). No clock data is exchanged here (S2: the guest owns its timeline).

## Security notes

- The token interceptor is the only authz gate; handlers assume an authenticated
  `case_id`. A guest can only ever touch its **own** case's data (enforced by
  binding token→case at mint time).
- The broker never returns real secrets to a guest. `allowed_models` and ingest
  URLs are the only "capabilities" leaked, all non-sensitive.
- Resource abuse (huge uploads) is bounded by `IngestLimits` + the host disk
  quota in `qwanban-artifacts`.

## Interfaces (exported)

- `qwanban-proto` crate: generated tonic client+server for `CaseControl`,
  `Ingest`; shared message types listed above.
- Rust host API (`qwanban-broker`):
  - `Broker::open_case(...) -> case_token` (called by orch, 7.1.4)
  - `Broker::close_case(case_id)` (7.12.10)
  - `Broker::subscribe_transitions() -> Stream<CaseTransition>` (for orch)
  - `Broker::case_registry()` (read model shared with `qwanban-inference`)

## Testing

- **Unit:** token interceptor (valid/expired/wrong-case); offset dedupe; ack/resume.
- **Integration (loopback, no VM):** a mock guest drives 7.2 → 7.5 → 7.6 → 7.12
  end-to-end against a real broker + temp artifact store; assert timeline join
  works (a breadcrumb's `timeline_ns` lands inside the covering video segment).
- **Fault injection:** kill transcript stream mid-batch, assert resume with no
  gap/dup; heartbeat timeout → `Error(guest_lost)`.
- **Conformance fixture:** publish a `proto` golden + a recorded message trace
  that every other component's mock can replay.

## Open items (delegated decisions)

- gRPC vs. HTTP/2+JSON for ingest (proto chosen here; revisit if guest binary
  size matters).
- Whether `CaseControl` (orch↔broker) runs over UDS/in-process vs. localhost TLS.

