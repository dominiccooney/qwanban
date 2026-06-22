# qwanban — Development Workflow

## Environment constraint: we develop *inside a VM*

We cannot run/develop directly on the Hyper-V **host**. Practical consequences:

- **No live Hyper-V** during normal development. Anything that touches the real
  hypervisor (`qwanban-hyperv`), real TCP bootstrap, real Desktop Duplication / DXGI,
  real `SendInput`/`uinput`, or a real LM Studio is **integration-gated** behind
  `#[ignore]` + a `QWAN_HOST_TESTS=1` env guard, and is only exercised on a real
  host later.
- Therefore **everything must be designed test-first behind traits** so the bulk
  of logic is covered by **unit tests that run anywhere** (in the dev VM, in CI).
  Each component doc's "Interfaces" section already defines the seam:
  - hyperv-driver → `HyperVDriver` trait (mock in tests)
  - input-injection → `InputBackend`/`InputSink` (mock backend)
  - video-capture-encode → `FrameSource` + capture/encoder traits (synthetic
    frame source; tiny test encoder or golden bytes)
  - broker-protocol → in-process tonic server + a **mock guest**
  - mitm-proxy / inference-router → loopback fake upstreams
- The **broker integration harness** (`broker-protocol.md` Testing) is the
  workhorse: a mock guest drives 7.2→7.5→7.6→7.7→7.12 with **no VM at all**, so
  end-to-end correctness (esp. the S2 timeline-join bookkeeping) is verified in
  the dev VM.

**Rule of thumb:** if a test needs the host, it's `#[ignore]`d and documented;
the component is still considered "done" (S8) only if its non-host unit tests
pass and its host-gated tests at least compile.

## Git workflow: worktrees + merge per component

Components are designed to be implemented in parallel by separate agents. Use
**git worktrees** (one per component branch) off `master`, then merge back.

### Layout

```
qwanban/                     # main worktree (master): docs, shared crates, integration
../qwanban-wt/
  broker-protocol/           # branch feat/broker-protocol
  hyperv-driver/             # branch feat/hyperv-driver
  mcp-server/                # branch feat/mcp-server
  …                          # one per docs/components/*.md
```

### Commands

```bash
# from the main worktree (master)
git worktree add ../qwanban-wt/broker-protocol -b feat/broker-protocol
git worktree add ../qwanban-wt/hyperv-driver   -b feat/hyperv-driver
# … one per component

# work happens in each worktree independently; commit there.

# integrate (fast-forward-friendly): rebase on master, then merge --no-ff
cd ../qwanban-wt/broker-protocol
git fetch && git rebase master
cd ../../qwanban
git merge --no-ff feat/broker-protocol
git worktree remove ../qwanban-wt/broker-protocol   # when merged
```

### Merge order = the build order

Follow the dependency-aware order in
[`components/README.md`](components/README.md) §"Build order":

1. `feat/broker-protocol` (defines `qwanban-proto`; everything imports it) →
   merge **first**.
2. `feat/hyperv-driver`, `feat/agent-lifecycle` (parallel).
3. `feat/mcp-server`, `feat/input-injection`, `feat/video-capture-encode`,
   `feat/breadcrumbs-transcript` (parallel).
4. `feat/mitm-proxy`, `feat/inference-router` (parallel).
5. `feat/artifact-store-and-clipping` + web.

### Reducing merge conflicts

- **One crate per component** (workspace members) → branches touch disjoint dirs.
- The shared `qwanban-proto` is owned solely by `feat/broker-protocol`; other
  branches depend on it as a path dep and **never edit it** (per the
  cross-component interface ownership table). If a downstream branch needs a
  proto change, it goes through `feat/broker-protocol` first.
- Each branch may only add to the top-level workspace `Cargo.toml`'s `members`
  list — keep those additions on separate lines to avoid conflicts.

### Definition of mergeable

A component branch merges to `master` only when (S8):
1. it implements its "Sequence coverage" steps,
2. conforms to S1–S7,
3. its exported interfaces match what dependents import,
4. its non-host unit tests pass; host-gated tests compile.

## Commit hygiene

- Conventional-ish messages scoped by component, e.g.
  `broker: add Register/Heartbeat services`, `video: keyframe-aligned segmenter`.
- Land the doc that motivates a change in the same train as the code (docs live
  in `master`; component branches may amend their own doc's "Open items" as they
  resolve them, merged back like code).
