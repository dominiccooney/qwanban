# Component: Breadcrumbs & Transcript (`qwanban-guest` + `qwanban-broker`)

> Owns the breadcrumb model and the transcript stream (the agent's narrated,
> timeline-stamped record), including durability/resume. Read
> [`README.md`](README.md) §S1–S8. Implements design.md §6.2, §4.3.

## Purpose & scope

The transcript is the **ordered, durable, timeline-stamped** record of a case:
breadcrumbs, Cline tool I/O echoes, and log lines. It is the join partner of the
video (via `timeline_ns`) and the backbone of the web report. Owns
`TranscriptEntry`, `BreadcrumbIn`, `Breadcrumb`, and the breadcrumb `kind` enum.

## Sequence coverage

Owns: **7.5.2–7.5.8** (emit, stamp, exfil, ack), the breadcrumb-id allocation in
**7.5.3**, the timeline stamping relied on by 7.6/7.7/7.9, and the transcript
flush at **7.12.3–7.12.5**.

## Dependencies

- Guest: the case-local `Timeline` handle (S2; owned by video-capture-encode),
  broker client (`AppendTranscript`).
- Host: broker `Ingest.AppendTranscript` → `qwanban-artifacts` (durable append).
- Consumers: clipping (`from/to` breadcrumb resolution), web report.

## Data model (owner)

```rust
pub enum BreadcrumbKind {
    StepBegin, StepEnd, Action, Assert,
    ReproConfirmed, FixVerified, Note, Error,
}
pub struct BreadcrumbIn {            // from MCP / qwan internals
    pub label: String,
    pub kind: BreadcrumbKind,
    pub data: serde_json::Value,     // small structured payload (selector, expected/actual…)
}
pub struct Breadcrumb {              // stamped, durable
    pub breadcrumb_id: u64,          // case-monotonic 1..N (S1)
    pub timeline_ns: i64,            // S2 join key
    pub wall_clock_rfc3339: String,  // metadata only
    pub label: String,
    pub kind: BreadcrumbKind,
    pub data: serde_json::Value,
}
```

Transcript entries are a tagged union so video sync works on a single ordered
stream:

```proto
message TranscriptBatch {
  string case_id = 1;
  uint64 from_seq = 2;                  // first entry's seq in this batch
  repeated TranscriptEntry entries = 3;
}
message TranscriptEntry {
  uint64 seq = 1;                       // per-case contiguous (S1)
  int64 timeline_ns = 2;
  oneof body {
    Breadcrumb breadcrumb = 3;
    LogLine log = 4;                    // {source: cline_stdout|cline_stderr|qwan, text}
    ToolIo tool_io = 5;                 // {tool, args_json, result_json} echo of MCP calls
  }
}
message AppendAck { uint64 up_to_seq = 1; }
```

## Guest behavior

- **Breadcrumb emit (7.5.2–7.5.5):** assign `breadcrumb_id` (monotonic),
  `timeline_ns = now()` (S2), append a `TranscriptEntry{breadcrumb}` to the
  transcript queue, return `Breadcrumb` to the caller (MCP) synchronously. Keep a
  **local breadcrumb table** `{breadcrumb_id -> timeline_ns}` for clip resolution
  (7.7.3) without a round-trip.
- **Tool I/O echo:** the MCP server feeds each tool call/result here as `ToolIo`
  so the report shows what the agent did, interleaved with breadcrumbs.
- **Logs:** the supervisor pipes Cline stdout/stderr + qwan logs as `LogLine`s.
- **Ordering & seq:** a single writer assigns `seq` so the stream is totally
  ordered per case; `(case_id, seq)` is the idempotency key.
- **Exfil (7.5.6):** batch entries (size/time-bounded) over `AppendTranscript`;
  on `AppendAck{up_to_seq}` drop acked entries; on disconnect, reopen and resume
  from `up_to_seq+1`. Spool to local disk if backlog grows (never block emit).

## Host behavior

- Broker `AppendTranscript` handler: dedupe by `seq`, append durably via
  artifacts (ordered log per case), return `AppendAck`. Maintains the
  **breadcrumb index** `{breadcrumb_id -> (seq, timeline_ns)}` and a
  `kind`-filtered view (for the report's "jump to repro" affordance).
- On migration (7.11), the broker continues the **same** seq/breadcrumb counters
  for the job so the concatenated timeline is unbroken (S1).

## Interfaces (exported)

```rust
pub trait BreadcrumbSink: Send + Sync {          // consumed by mcp-server
    async fn emit(&self, b: BreadcrumbIn) -> Result<Breadcrumb>;
    async fn make_clip(&self, from_ts: i64, to_ts: i64, label: String) -> Result<ClipAsset>;
    fn resolve(&self, bc_ref: BreadcrumbRef) -> Result<i64 /*timeline_ns*/>; // id|"now"|"-30s"
}
pub trait TranscriptSink: Send + Sync {          // consumed by supervisor/mcp
    fn append_log(&self, src: LogSource, text: String);
    fn append_tool_io(&self, tool: &str, args: Value, result: Value);
}
```

(`make_clip` delegates to artifact-store-and-clipping; it lives on this trait
because the MCP server already holds a `BreadcrumbSink` and clips are addressed
by breadcrumbs. `ClipAsset` owned by that doc.)

## Testing

- **Unit:** monotonic `breadcrumb_id`/`seq`; local breadcrumb table resolution
  for `id`/`now`/relative refs; batch/ack/resume dedupe.
- **Integration (mock broker):** emit interleaved breadcrumbs+logs+tool_io,
  disconnect mid-batch, assert resumed stream is gap-free and correctly ordered.
- **Join test (with video doc):** a `ReproConfirmed` breadcrumb's `timeline_ns`
  falls within the covering video segment's `[first_ts,last_ts]`.

## Open items

- Max `data` payload size / redaction policy for breadcrumb `data` (keep small;
  full state goes to logs).
