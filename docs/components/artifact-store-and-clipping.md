# Component: Artifact Store, Clipping & Web Report (`qwanban-artifacts` + `qwanban-web`)

> Owns durable storage of recordings/transcripts/logs/clips, the indices that
> join them, clip cutting, and the read-only web report. Read
> [`README.md`](README.md) §S1–S8. Implements design.md §6.3, §6.4.

## Purpose & scope

The host-side persistence + presentation layer:

- **Artifact store:** content-addressed, compressed storage of video segments,
  transcript log, logs, and clips, per case/job, plus the **segment index** and
  **breadcrumb index**.
- **Clipping:** cut a labeled video clip between two `timeline_ns` points, on
  keyframe boundaries, for evidence in PRs/reports.
- **Web report:** a read-only page per job with a synchronized transcript↔video
  scrubber, breadcrumb timeline, and attached clips.

It owns the `ClipRequest`/`ClipResponse`/`ClipAsset` types and the **on-disk
storage layout**.

## Sequence coverage

Owns: storage side of **7.5.7, 7.6.5, 7.12.5**; clipping **7.7.5–7.7.8**; web
report **7.9.1–7.9.7**.

## Dependencies

- Broker ingest hands it transcript batches (breadcrumbs-transcript), video
  segments + index (video-capture-encode), and clip requests.
- Web reads everything back. A demux/remux tool (FFI to ffmpeg libs or a Rust
  mp4/webm muxer) for clip cutting.

## Storage layout

```
<artifact_root>/
  jobs/<job_id>/job.json                # job meta, cases[], outcome
  cases/<case_id>/
    manifest.json                       # copy (no secrets)
    transcript.log                      # append-only TranscriptEntry (length-prefixed/JSONL)
    breadcrumb.index                    # breadcrumb_id -> (seq, timeline_ns, kind, label)
    video/
      seg-000000.m4s ...                # fragmented segments (or .webm)
      segment.index                     # segment_idx -> {first_ts,last_ts,kf,codec,off,len}
    logs/cline.log, logs/qwan.log
    clips/<clip_id>.mp4                  # cut evidence
    clips/<clip_id>.json                # {from_ts,to_ts,label,source_segments[]}
```

- **Content-addressing/compression:** large blobs stored compressed; optional
  dedupe by content hash. Per-case **disk quota** enforced here (bounds a hostile
  guest, §13). 
- For a migrated job (multiple cases), the **job timeline** is the concatenation
  of cases' segment/transcript ranges (counters are continuous per S1), so the
  report renders one unbroken timeline.

## Indices (the joins)

- **segment.index** (owned by video doc, persisted here): maps `timeline_ns`
  ranges → byte ranges. Used by clipping + web seek.
- **breadcrumb.index** (owned by transcript doc, persisted here): maps
  `breadcrumb_id` → `(seq, timeline_ns, kind, label)`; powers "jump to repro".

## Clipping (7.7.5–7.7.8)

```
input: ClipRequest{ case_id, clip_id, from_ts, to_ts, label }
1. via segment.index, find segments covering [from_ts, to_ts]
2. if from_ts/to_ts already keyframe-aligned at segment edges -> remux (copy, no re-encode)
3. else -> re-encode only the boundary GOP(s) to land exact cut points
4. write clips/<clip_id>.mp4 + sidecar json; compute web_url
5. return ClipResponse{ clip_id, web_url, exact_from_ts, exact_to_ts, duration }
```

- **Idempotent** by `clip_id` (client-generated, S1): a repeat request returns the
  existing asset.
- Default container MP4 (H.264) for broad browser/PR embedding; if source is AV1/
  WebM, transcode the clip to MP4 for compatibility (configurable).

### Types (owner)

```proto
message ClipRequest { string case_id=1; string clip_id=2; int64 from_ts=3;
                      int64 to_ts=4; string label=5; }
message ClipResponse { string clip_id=1; string web_url=2;
                       int64 exact_from_ts=3; int64 exact_to_ts=4; double duration_s=5; }
```
```rust
pub struct ClipAsset { pub clip_id: String, pub web_url: String,
                       pub path: PathBuf, pub from_ts: i64, pub to_ts: i64 }
```

## Web report (`qwanban-web`, 7.9)

- Per-job page (`GET /jobs/{job_id}`): video player + transcript pane +
  breadcrumb timeline + clip list + outcome (result, pr_url).
- **Sync:** clicking a transcript breadcrumb seeks the player to its
  `timeline_ns`; playing the video highlights the current transcript entry. Join
  via the two indices.
- **Video serving:** `GET /jobs/{job_id}/video?from_ts&to_ts` maps to segment
  byte ranges (HTTP range requests) so seeking streams only needed fragments.
- Read-only; served from the artifact store; suitable for linking in PRs (clip
  `web_url`s are stable). Auth: host-local / simple token (out of guest scope).

## Interfaces (exported)

```rust
pub trait ArtifactStore: Send + Sync {
    async fn append_transcript(&self, case_id:&str, batch: TranscriptBatch) -> Result<u64>;
    async fn put_video_segment(&self, case_id:&str, seg: VideoChunkAssembled) -> Result<()>;
    fn segment_index(&self, case_id:&str) -> SegmentIndex;
    fn breadcrumb_index(&self, case_id:&str) -> BreadcrumbIndex;
    async fn make_clip(&self, req: ClipRequest) -> Result<ClipResponse>;
    async fn finalize_case(&self, case_id:&str, result: CaseResult) -> Result<()>;
}
```

## Testing

- **Unit:** segment lookup for a ts-range; keyframe-aligned remux vs. boundary
  re-encode decision; clip idempotency.
- **Integration:** ingest synthetic segments+transcript, request a clip spanning
  3 segments, assert duration ≈ to−from and it’s playable; assert `exact_*` land
  on/inside requested bounds.
- **Web:** range-request seeking returns correct bytes; breadcrumb→seek mapping;
  multi-case (migrated) job renders one timeline.
- **Quota:** exceeding per-case disk quota is rejected/trimmed gracefully.

## Open items

- Clip transcode policy (always MP4 vs. keep source codec).
- Retention/GC policy for archived jobs (post-v1).
