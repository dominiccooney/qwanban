# qwanban — Sequence Diagrams (Section 7)

> Companion to [`design.md`](design.md). This document specifies the **common
> interactions** between qwanban components as UML-style sequence diagrams.
> Each message is numbered `7.<scenario>.<step>` so component sub-design docs
> (see [`components/`](components/README.md)) can reference exact steps they must
> implement. When you implement a component, your work must satisfy every step in
> which your participant appears.

## 7.0 Participants (legend)

Host side (trusted):

| Alias | Component | Crate / artifact |
|-------|-----------|------------------|
| `caller` | CLI user or Rust API caller | `qwanban-cli` / lib |
| `orch` | Orchestrator + job scheduler + case state machine | `qwanban-core` |
| `hyperv` | Hyper-V driver | `qwanban-hyperv` |
| `broker` | Broker: gRPC control plane + transcript/video/clip ingest | `qwanban-broker` |
| `infer` | Inference endpoint (OpenAI-compatible HTTP) | `qwanban-inference` |
| `vault` | Secret vault (host file) | part of `qwanban-broker` |
| `proxy` | MITM HTTPS proxy | `qwanban-proxy` |
| `artifacts` | Artifact store (recordings, clips, logs) | `qwanban-artifacts` |
| `web` | Web report server | `qwanban-web` |
| `lmstudio` | LM Studio (fixed models) | external |
| `github` | GitHub / other pinned upstreams | external |

Guest side (untrusted VM):

| Alias | Component | Crate / artifact |
|-------|-----------|------------------|
| `guest` | qwan agent (a.k.a. "qwan-guest") | `qwanban-guest` |
| `cuxec` | computer-use executor: Cline agent-loop adapter + input/screenshot backend (runs inside `guest`) | `qwanban-guest` (computer module) |
| `mcp` | qwan MCP server (qwan-only tools: breadcrumb/clip/handoff/finish; runs inside `guest`) | `qwanban-guest` (mcp module) |
| `cline` | Cline agent (tester/fixer), patched to use Anthropic computer-use beta | Cline SDK/CLI/ext |
| `sut` | Software under test | from base image |

> Computer control uses **Anthropic's built-in computer-use tool** (`cline ↔
> cuxec`), NOT the qwan MCP. The qwan MCP carries only qwan-specific tools.

> Mapping to the user's shorthand: "qwan-host" ≈ `broker`/`orch`; "qwan-guest" ≈
> `guest`.

## 7.0.1 Conventions used in these diagrams

- `A -> B` synchronous request (caller waits); `A --> B` response/return;
  `A ->> B` async/fire-and-forget or stream item; `A ->* B` start of a long-lived
  stream.
- `[alt]`, `[opt]`, `[loop]`, `[par]` are UML combined fragments.
- All `guest -> broker` traffic rides the gRPC control plane or bulk streams
  defined in [`components/broker-protocol.md`](components/broker-protocol.md).
- All `cline -> mcp` traffic is MCP tool calls over guest loopback
  ([`components/mcp-server.md`](components/mcp-server.md)).
- Identifiers (`job_id`, `case_id`, `breadcrumb_id`, `clip_id`) and the **case
  timeline / clock model** are defined in
  [`components/README.md`](components/README.md) §"Shared contracts".


---

## 7.1 Job submission → case provisioning → qwan agent push

Covers: `design.md` §5.1–5.4, §5.7. Implemented by `orch`, `hyperv`, `broker`,
`guest`. See [`hyperv-driver.md`](components/hyperv-driver.md),
[`agent-lifecycle.md`](components/agent-lifecycle.md).

```
7.1.1  caller -> orch     : submit(JobSpec{ kind, base_image, git_ref, script|report, note, caps? })
7.1.2  orch -> orch       : validate spec; resolve base image path from registry; allocate job_id
7.1.3  orch -> orch       : if a free slot (< max_concurrent_cases): allocate case_id;
                            else reject submit with ResourceExhausted (NO queue, §5.8)
7.1.4  orch -> broker     : open_case(case_id, job_id, manifest, resource_caps)
7.1.5  broker --> orch    : case_token (per-case bearer secret for guest auth)
7.1.6  orch -> hyperv     : create_case_vm(case_id, base_vhd, caps, vswitch="qwan-internal")
7.1.7  hyperv -> hyperv   : create differencing AVHDX on base parent
7.1.8  hyperv -> hyperv   : define VM (vcpu/mem/dynamic-mem limits), attach NIC + hvsocket
7.1.9  hyperv --> orch    : vm_handle{ vm_id, hvsocket_vmid }
7.1.10 orch -> hyperv     : start_vm(vm_id)
7.1.11 hyperv ->> orch    : state=Booting
7.1.12 orch ->* guest     : (over hvsocket) wait for bootstrap listener ready
        note: base image ships only the tiny stable `qwan-stub` hvsocket loader (no SSH)
7.1.13 orch -> guest      : push_agent(qwan-guest binary for OS/arch, sha256)   [§5.7]
7.1.14 guest --> orch     : agent_pushed(ok, version)
7.1.15 orch -> guest      : write_files(manifest.json, case_token, broker_endpoint, proxy_ca_fpr, dummy_keys)
7.1.16 orch -> guest      : launch_agent()
7.1.17 hyperv/orch ->> orch : state=QwanAgentPushed
```

Failure handling:

```
7.1.E1 [alt boot timeout] hyperv --> orch : state=Error(boot_timeout)
7.1.E2                     orch -> hyperv  : destroy_case_vm(vm_id)   (unless --hold-on-error)
7.1.E3 [alt push hash mismatch] guest --> orch : agent_pushed(err, hash_mismatch)
7.1.E4                     orch -> orch    : retry push up to N; else Error
```

---

## 7.2 qwan agent boot → register with broker → start Cline agent

Covers: §4.3, §5.4. Implemented by `guest`, `broker`, `mcp`, `cline`. See
[`agent-lifecycle.md`](components/agent-lifecycle.md),
[`mcp-server.md`](components/mcp-server.md).

```
7.2.1  guest -> guest     : read manifest.json + case_token + broker_endpoint
7.2.2  guest -> broker    : Register(case_id, case_token, guest_info{os, arch, screen_dims, agent_version})
7.2.3  broker --> guest   : Registered(timeline_offset_ns, ingest_urls{transcript, video, clips}, allowed_models[])
        note: NO clock sync (S2). timeline_offset_ns is presentation-only (0 unless OS-migration successor).
7.2.5a guest -> guest     : start capture pipeline FIRST → sets case timeline origin t0 = monotonic_now()
                            (the single guest-local clock; timeline_ns = monotonic_now() - t0)
7.2.4  guest -> mcp       : start MCP server on guest loopback (127.0.0.1:PORT) with capabilities
7.2.5  guest -> guest     : start transcript sink (see 7.5), sharing the Timeline handle from 7.2.5a
7.2.6  guest -> cline     : spawn Cline agent with:
                            - task prompt (from manifest: QA script text / bug report + note)
                            - MCP config pointing at 127.0.0.1:PORT
                            - inference base_url=broker_endpoint/v1, api_key=DUMMY
7.2.6b guest -> cline     : also configure Anthropic computer-use beta + cuxec as the tool executor
7.2.7  cline -> mcp       : initialize (MCP handshake) ; list_tools
7.2.8  mcp --> cline      : tools[ breadcrumb, clip, request_intervention, request_os_migration, finish ]
                            note: computer-control is the native computer-use tool via cuxec (7.4), NOT MCP
7.2.9  guest ->> broker   : Heartbeat(case_id, status=Running)   [loop every Hb interval]
7.2.10 broker ->> orch    : state=ClineAgentReady -> Running
```

---

## 7.3 Inference request (Cline → host LM Studio / cloud)

Covers: §7. Implemented by `cline`, `broker`, `infer`, `vault`. See
[`inference-router.md`](components/inference-router.md).

Two paths exist; **7.3.A** (Broker endpoint) is primary, **7.3.B** (via MITM
proxy) covers tools that hardcode public endpoints.

### 7.3.A Broker inference endpoint (preferred)

```
7.3.A1 cline -> infer     : POST /v1/chat/completions  Authorization: Bearer DUMMY  {model, messages, stream}
7.3.A2 infer -> infer     : authenticate case via DUMMY->case binding; check model in allowed_models
7.3.A3 [alt model not allowed] infer --> cline : 403 model_not_allowed ; END
7.3.A4 infer -> vault     : resolve route(model) -> {target=lmstudio|cloud, real_key?}
7.3.A5 [alt target=lmstudio] infer -> lmstudio : POST /v1/chat/completions {model,...}
7.3.A6 [alt target=cloud]    infer -> cloud : POST ... Authorization: Bearer <REAL_KEY>
7.3.A7 lmstudio/cloud ->> infer : stream chunks (SSE)
7.3.A8 infer ->> cline    : stream chunks (SSE passthrough)
7.3.A9 infer ->> broker   : usage record(case_id, model, tokens) for audit/caps
```

### 7.3.B Via MITM proxy (third-party tool path)

```
7.3.B1 cline/tool -> proxy : CONNECT api.openai.com:443 (system proxy)
7.3.B2 proxy -> proxy      : TLS-terminate using leaf cert signed by qwan CA (guest trusts CA)
7.3.B3 proxy -> proxy      : host pin check (allowlist) ; [alt blocked] -> 403; END
7.3.B4 proxy -> proxy      : search→replace: scan req for known dummy bytes; swap dummy -> real secret
                              [alt no dummy found] -> forward verbatim (no injection)
7.3.B5 proxy -> upstream   : forward over validated TLS (real secret now in place)
7.3.B6 upstream ->> proxy  : response ; proxy ->> cline : response
7.3.B7 proxy ->> broker    : audit(case_id, host, method, path, bytes, which dummy matched)

---

## 7.4 QA step: computer control (Anthropic computer-use) + observation loop

Covers: §9.1–9.2. Implemented by `cline` + `cuxec` (the computer-use executor =
the in-guest agent-loop adapter + input/capture backend) and `sut`. See
[`input-injection.md`](components/input-injection.md) (the executor) and
[`video-capture-encode.md`](components/video-capture-encode.md) (screenshot
source). **This loop is the Anthropic computer-use tool — it does NOT go through
the qwan MCP** (see mcp-server.md decision).

`cuxec` = computer-use executor inside the qwan agent. Cline emits
`computer_20250124` tool_use blocks; the qwan agent-loop adapter runs them via
`cuxec` and returns `tool_result` blocks. Coordinates are in the **scaled API
resolution** (XGA/WXGA/FWXGA); `cuxec` scales API↔screen each step.

```
7.4.1  cline -> cuxec     : tool_use{ action="screenshot" }
7.4.2  cuxec -> guest     : FrameSource.capture_now()  (latest keyframe from capture pipeline)
7.4.3  guest --> cuxec    : raw frame + frame_ts (timeline_ns)
7.4.4  cuxec -> cuxec     : downscale screen->API target (ScalingSource::COMPUTER)
7.4.5  cuxec --> cline    : tool_result{ image(base64) }   # agent loop feeds back to model
7.4.6  cline -> cline     : (LLM) decide next action
7.4.7  cline -> cuxec     : tool_use{ action="left_click", coordinate=[x,y], key? }
7.4.8  cuxec -> cuxec     : scale coord API->screen (ScalingSource::API); reject if OOB
7.4.9  cuxec -> sut       : OS injection (Win: SendInput; Linux: uinput/XTEST)
7.4.10 cuxec -> cuxec     : stamp injected_ts (timeline_ns); echo ToolIo to transcript (7.5)
7.4.11 cuxec --> cline    : tool_result{ screenshot after settle delay }  # observe -> loop to 7.4.6
```

Note: the qwan agent MAY auto-emit a low-detail breadcrumb (`kind=Action`, via
the transcript sink — see 7.5) for each executed action so the recording stays
densely indexed even if the agent doesn't explicitly mark steps. This is a qwan
concern bolted onto `cuxec`, independent of the MCP `breadcrumb` tool.

---

## 7.5 Breadcrumb emission + transcript exfiltration

Covers: §6.2, §4.3. Implemented by `cline`, `mcp`, `guest`, `broker`,
`artifacts`. See [`breadcrumbs-transcript.md`](components/breadcrumbs-transcript.md).

```
7.5.1  cline -> mcp       : breadcrumb(label="step.begin: add to cart", kind=StepBegin, data{...})
7.5.2  mcp -> guest       : emit_breadcrumb(BreadcrumbIn{label, kind, data})
7.5.3  guest -> guest     : assign breadcrumb_id (case-monotonic), stamp timeline_ns = monotonic_now() - t0  (S2)
7.5.4  guest --> mcp      : breadcrumb_ack(breadcrumb_id, timeline_ns)
7.5.5  mcp --> cline      : ok(breadcrumb_id)
7.5.6  guest ->> broker   : TranscriptAppend(case_id, seq, entries[ breadcrumb | log | tool_io ])  [batched/stream]
7.5.7  broker -> artifacts: append transcript segment (durable, ordered by seq)
7.5.8  broker --> guest   : ack(up_to_seq)        # at-least-once; guest retransmits gaps
```

The transcript stream interleaves breadcrumbs, Cline tool I/O echoes, and log
lines; ordering key is `(case_id, seq)`. `timeline_ns` on each breadcrumb is the
join key to video (7.6) and clips (7.7).

---

## 7.6 Continuous video capture → encode → upload

Covers: §6.1. Implemented by `guest`, `broker`, `artifacts`. See
[`video-capture-encode.md`](components/video-capture-encode.md).

Runs continuously from 7.2.5 until case teardown.

```
7.6.1  guest ->* guest    : capture loop: grab frame from display @ target_fps, stamp capture_ts
7.6.2  guest -> guest     : encoder: feed frames -> H.264/AV1, GOP=keyframe_interval
7.6.3  guest -> guest     : muxer: write fragmented MP4/WebM, one fragment per segment_seconds
                            each fragment header carries first_ts/last_ts (timeline ns) + keyframe flag
7.6.4  guest ->> broker   : VideoSegment(case_id, segment_idx, first_ts, last_ts, keyframe_aligned, bytes)
7.6.5  broker -> artifacts: store segment under case_id; index (segment_idx -> [first_ts,last_ts])
7.6.6  broker --> guest   : ack(segment_idx)     # backpressure: guest buffers/spools to disk if unacked
7.6.7  [alt host-side encode] guest ->> broker : RawFrames(...) ; broker -> (encoder) -> artifacts
        note: 7.6.7 only if guest lacks a usable encoder; default is guest-side encode (§6.1)
7.6.8  [opt] mcp -> guest : capture_frame_now()  (7.4.2) pulls the latest decoded keyframe for screenshots
```

The **segment index** (`segment_idx -> [first_ts,last_ts]`, keyframe alignment)
is the contract clipping (7.7) and the web player (7.9) rely on.

---

## 7.7 Clipping (evidence cut between breadcrumbs)

Covers: §6.3. Implemented by `cline`, `mcp`, `guest`, `broker`, `artifacts`. See
[`artifact-store-and-clipping.md`](components/artifact-store-and-clipping.md).

```
7.7.1  cline -> mcp       : clip(from_breadcrumb=B1, to_breadcrumb=B2, label="repro: wrong total")
7.7.2  mcp -> guest       : make_clip(from=B1, to=B2, label)
7.7.3  guest -> guest     : resolve B1.timeline_ns, B2.timeline_ns (local breadcrumb table)
7.7.4  guest ->> broker   : ClipRequest(case_id, clip_id, from_ts, to_ts, label)
7.7.5  broker -> artifacts: locate segments covering [from_ts,to_ts] via segment index
7.7.6  artifacts -> artifacts : cut on keyframe boundaries; remux if aligned, else re-encode boundary GOPs
7.7.7  artifacts --> broker : clip asset{ clip_id, path, duration, exact_from_ts, exact_to_ts }
7.7.8  broker --> guest   : clip_ready(clip_id, web_url)
7.7.9  guest --> mcp      : clip_ack(clip_id, web_url)
7.7.10 mcp --> cline      : ok(clip_id, web_url)   # agent embeds web_url in PR body later
```


---

## 7.8 Bug fix → open PR (git/PR proxy, dummy→real token)

Covers: §10. Implemented by `cline`, `proxy`/git proxy, `vault`, `github`,
`broker`. See [`mitm-proxy.md`](components/mitm-proxy.md).

The guest's git/`gh` uses a **dummy** token; the host rewrites it.

```
7.8.1  cline -> sut        : git checkout -b fix/1234 ; commit changes (local in VM)
7.8.2  cline -> proxy      : git push / gh pr create  (HTTPS to github.com via system proxy, DUMMY token)
7.8.3  proxy -> proxy      : host pin check github.com (allowlist)
7.8.4  proxy -> proxy      : search→replace: swap dummy token bytes -> real github_token
                              (works the same in Basic-auth for git push and Bearer for gh API)
7.8.5  proxy -> github     : forward push / create PR (body includes clip web_urls from 7.7)
7.8.6  github ->> proxy    : PR created {pr_url}
7.8.7  proxy ->> cline     : success {pr_url}
7.8.8  proxy ->> broker    : audit(case_id, github.com, action=pr_create, which dummy matched)
7.8.9  cline -> mcp        : finish(result=Fixed, pr_url, summary)   # see 7.12
```

---

## 7.9 Web report viewing (post-hoc, read-only)

Covers: §6.4. Implemented by `web`, `artifacts`. See
[`artifact-store-and-clipping.md`](components/artifact-store-and-clipping.md).

```
7.9.1  caller -> web       : GET /jobs/{job_id}        (or `qwanban report open`)
7.9.2  web -> artifacts     : load transcript + breadcrumb index + segment index + clips
7.9.3  web --> caller       : HTML page (player + transcript pane + breadcrumb timeline)
7.9.4  caller -> web        : GET /jobs/{job_id}/video?from_ts&to_ts   (range/seek)
7.9.5  web -> artifacts      : map [from_ts,to_ts] -> segments; return byte ranges
7.9.6  web --> caller        : fragmented MP4/WebM byte stream
7.9.7  caller -> caller      : click transcript breadcrumb -> player.seek(breadcrumb.timeline_ns)
```

---

## 7.10 Human intervention (hold the VM)

Covers: §5.5. Implemented by `cline`, `mcp`, `guest`, `broker`, `orch`,
`hyperv`.

```
7.10.1 cline -> mcp        : request_intervention(reason="stuck: needs 2FA")
7.10.2 mcp -> guest        : intervention(reason)
7.10.3 guest ->> broker    : Handoff(case_id, kind=Intervention, reason)
7.10.4 broker ->> orch     : intervention_requested(case_id, reason)
7.10.5 orch -> hyperv      : checkpoint_vm(vm_id, "pre-intervention")
7.10.6 orch -> orch        : state=Held (disable auto-teardown timer)
7.10.7 orch ->> caller     : notify(case_id HELD, connect via VMConnect/RDP/SSH)
7.10.8 mcp --> cline       : ok(held)  # agent may idle/await or exit; VM persists
        ... maintainer acts ...
7.10.9 caller -> orch      : resume(case_id) | destroy(case_id)
```

---

## 7.11 OS migration (Linux ⇄ Windows mid-case)

Covers: §5.6. Implemented by `cline`, `mcp`, `guest`, `broker`, `orch`,
`hyperv`.

```
7.11.1 cline -> mcp        : request_os_migration(target_os="windows", reason)
7.11.2 mcp -> guest        : os_migration(target_os)
7.11.3 guest ->> broker    : Handoff(case_id, kind=OsMigration, target_os, portable_state_ref)
7.11.4 guest -> broker     : upload portable state (repo working tree diff, manifest, accumulated artifacts)
7.11.5 broker ->> orch     : migration_requested(case_id, target_os)
7.11.6 orch -> hyperv      : create_case_vm(case_id', target_base_vhd, caps)    # new case, same job
7.11.7 orch -> guest'      : push_agent + write_files (manifest + clock continuity ref)  [7.1.13-16]
7.11.8 guest' -> broker    : Register(...) ; broker continues SAME job timeline (new segment range)
7.11.9 broker -> guest'    : rehydrate portable state into VM working dir
7.11.10 orch -> hyperv     : (old VM) hold briefly then destroy (or hold if intervention)
7.11.11 guest' -> cline'   : resume task with migrated context
```

---

## 7.12 Case completion → teardown → archive

Covers: §5.5, §12. Implemented by `cline`, `mcp`, `guest`, `broker`, `orch`,
`hyperv`, `artifacts`.

```
7.12.1 cline -> mcp        : finish(result=Pass|Fail|Fixed|Unreproducible, summary, pr_url?)
7.12.2 mcp -> guest        : finalize(result, summary)
7.12.3 guest -> guest      : flush encoder (last fragment), flush transcript queue
7.12.4 guest ->> broker    : final VideoSegment(s) + TranscriptAppend(final seq) + CaseResult(result, summary)
7.12.5 broker -> artifacts : finalize indices (transcript complete, segment index closed, clips linked)
7.12.6 broker ->> orch     : case_completed(case_id, result)
7.12.7 orch -> orch        : state -> Completed|Failed|Error
7.12.8 [alt not held]      orch -> hyperv : stop_vm + destroy_case_vm (delete AVHDX)
7.12.9 [alt hold-on-error] orch -> hyperv : checkpoint + keep
7.12.10 orch -> broker     : close_case(case_id) (invalidate case_token, allowed_models)
7.12.11 orch ->> caller    : JobOutcome(result, report_url, repros[], pr_url?)
```

---

## 7.13 Coverage matrix (scenario → component doc)

| Scenario | Primary component doc(s) |
|----------|--------------------------|
| 7.1 provision + push | hyperv-driver, agent-lifecycle |
| 7.2 register + start cline | agent-lifecycle, mcp-server, broker-protocol |
| 7.3 inference | inference-router, mitm-proxy |
| 7.4 computer control (Anthropic computer-use, via cuxec — NOT mcp) | input-injection, video-capture-encode, agent-lifecycle (agent-loop adapter) |
| 7.5 breadcrumbs/transcript | breadcrumbs-transcript, broker-protocol |
| 7.6 video pipeline | video-capture-encode, broker-protocol |
| 7.7 clipping | artifact-store-and-clipping |
| 7.8 PR via proxy | mitm-proxy |
| 7.9 web report | artifact-store-and-clipping (+ web) |
| 7.10 intervention | agent-lifecycle, hyperv-driver, broker-protocol |
| 7.11 OS migration | agent-lifecycle, hyperv-driver, broker-protocol |
| 7.12 teardown | hyperv-driver, broker-protocol, artifact-store-and-clipping |

Every component doc has a "Sequence coverage" section listing exactly which
`7.x.y` steps it owns.

```
