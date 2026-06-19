# Component: Video Capture & Encode (`qwanban-guest` capture + `qwanban-broker` ingest)

> Owns continuous screen capture, encoding, segmentation, upload, and the
> screenshot-pull used by the MCP `screenshot` tool. Read
> [`README.md`](README.md) §S1–S8. Implements design.md §6.1.

## Purpose & scope

Continuously record the guest display, encode + segment it, stamp every fragment
in `timeline_ns`, and stream segments to the broker for storage/indexing. Also
serve **single-frame pulls** to the MCP server for `screenshot` (7.4.2). Owns the
`VideoSegment`/`VideoChunk`/`RawFrames` types and the **segment index** contract.

Per design.md §6.1, **compression may happen in the guest or on the host** —
default guest-side; host-side encode is a fallback for guests lacking an encoder.

## Sequence coverage

Owns: **7.2.5** (start pipeline), **7.4.2–7.4.3, 7.4.13/7.6.8** (screenshot
pull), **7.6.1–7.6.7** (capture→encode→segment→upload), broker side of
**7.6.4–7.6.6**, and the flush at **7.12.3**.

## Dependencies

- Guest: a capture source (OS), an encoder, the broker client (`UploadVideo`).
- **Owns the case timeline origin (S2):** this pipeline sets `t0 =
  monotonic_now()` at the first captured frame and exposes the `Timeline` handle
  (`now() -> timeline_ns = monotonic_now() - t0`) that the MCP/input/transcript
  subsystems all stamp against. No host clock is involved.
- Host: broker `Ingest.UploadVideo` handler → `qwanban-artifacts` (storage +
  index). Optional host-side encoder for the `RawFrames` fallback.
- Consumers of the index: artifact-store-and-clipping (7.7), web (7.9).

## Pipeline (guest-side, default)

```
[capture] -> [scaler/format] -> [encoder] -> [muxer/segmenter] -> [uploader]
                                   |                                  |
                            (latest keyframe cache for screenshots)   v
                                                                   broker
```

### Capture source

- **Windows:** Desktop Duplication API (DXGI `IDXGIOutputDuplication`) — efficient
  GPU-free frame grab of the desktop. Fallback: GDI `BitBlt` for odd sessions.
- **Linux:** PipeWire screencast (Wayland) or X11 `XShmGetImage`/`XComposite`.
  Chosen by environment; both yield raw frames + a capture timestamp.
- Target `fps` from manifest (`capture.fps`, default 5 — QA doesn't need 60fps;
  low fps keeps cost down). Frames stamped `capture_ts = now()` in timeline_ns.

### Encoder

- H.264 (baseline/main) or AV1 via a Rust-friendly encoder binding
  (e.g. `openh264`/`x264` FFI, or `rav1e` for AV1). Config: bitrate/CRF,
  `keyframe_interval` (GOP) tied to `segment_seconds` so **every segment starts
  on a keyframe** (critical for clip cutting + seek).
- Maintain a **latest-keyframe cache** (most recent fully decoded frame) for the
  MCP `screenshot` pull, so screenshots are cheap and don't perturb the stream.

### Muxer / segmenter

- Fragmented **MP4** (H.264) or **WebM** (AV1), one fragment per
  `segment_seconds`. Each segment is self-contained and **keyframe-aligned**.
- Each segment carries metadata: `segment_idx` (contiguous), `first_ts`,
  `last_ts` (timeline_ns), `keyframe_aligned=true`, codec, w/h.

### Uploader

- Streams `VideoChunk`s over `Ingest.UploadVideo` (broker-protocol streaming
  semantics: acks + resume by `segment_idx`, S3).
- **Backpressure/durability:** if the broker is slow/unreachable, spool segments
  to a local ring buffer on disk (bounded by `caps.disk`), resume on reconnect.
  Capture never blocks on upload.

## Types (owner)

```proto
message VideoChunk {
  string case_id = 1;
  int32 segment_idx = 2;
  int64 first_ts = 3; int64 last_ts = 4;   // timeline_ns
  bool keyframe_aligned = 5;
  string codec = 6;                         // "h264" | "av1"
  int32 width = 7; int32 height = 8;
  bytes data = 9;                            // one fragment (may span multiple chunks)
  bool last_chunk_of_segment = 10;
}
message VideoAck { int32 up_to_idx = 1; }   // resume point

// Fallback path (guest lacks encoder), §6.1 / 7.6.7:
message RawFrames { string case_id=1; int64 ts=2; int32 w=3; int32 h=4;
                    string pixfmt=5; bytes frame=6; }
```

### Segment index (host, owned here, stored by artifacts)

`segment_idx -> { first_ts, last_ts, keyframe_aligned, codec, byte_offset, len }`,
persisted per case. This is the **single source of truth** clipping and the web
player use to map a `timeline_ns` range to bytes.

## Host-side encode fallback (7.6.7)

If `capture.encode_where == "host"` (guest can't encode), the guest sends
`RawFrames`; a broker-side encoder produces the same segment format + index. The
**output contract is identical**, so downstream (clip/web) is unaffected.

## Screenshot pull (7.4.2)

`FrameSource::capture_now(fmt)` returns the latest cached keyframe transcoded to
PNG/JPEG + its `frame_ts`. Must be O(1)-ish and not stall the encoder
(serves from the keyframe cache, re-encodes a still).

## Interfaces (exported)

```rust
pub trait FrameSource: Send + Sync {        // consumed by mcp-server
    async fn capture_now(&self, fmt: ImgFmt) -> Result<(Bytes, i64)>; // (image, frame_ts)
}
pub struct CapturePipeline { /* start(), stop_and_flush(), health() */ }
```

Broker exports the `UploadVideo` handler + `SegmentIndex` read API for
artifacts/web.

## Testing

- **Unit:** segmenter keyframe alignment; `first_ts/last_ts` monotonic &
  contiguous; resume-by-idx dedupe.
- **Guest integration (gated):** capture a known animation, assert N segments,
  each decodable and keyframe-starting; `capture_now` returns a frame whose
  `frame_ts` ∈ current segment.
- **Backpressure:** stall the broker; assert spooling then clean resume with no
  index gaps.
- **Fallback parity:** run RawFrames→host-encode and assert identical index
  contract.

## Open items

- Encoder choice per OS (hardware-free): openh264 vs x264 FFI vs av1; measure CPU
  under the host resource caps.
- fps/bitrate defaults for readable text at low cost (tie to §15.3 bandwidth Q).
