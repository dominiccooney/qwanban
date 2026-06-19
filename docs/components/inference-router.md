# Component: Inference Routing (`qwanban-inference`)

> Owns the **model allowlist + route resolution** — *not* a server. Guests call
> inference directly over the vSwitch: LM Studio for local models (no key), and
> cloud providers via the **MITM proxy** (which does the dummy→real secret swap).
> Read [`README.md`](README.md) §S1–S8. Implements design.md §7, 7.3.A/B.

> **CORRECTION (user, mid-impl):** there is no inference server. LM Studio is
> local on the host and the cloud path already goes through the proxy. A separate
> listening process would duplicate the proxy's secret-swap and add a needless
> hop. This crate is now **pure routing logic** — a library the orchestrator +
> manifest builder call, not a daemon.

## Purpose & scope

`qwanban-inference` is a **library** (no `run(addr)` server) that:

1. Resolves the configured **routes** (model → LM Studio | cloud) from `qwanban.toml`.
2. Computes the **per-case model allowlist** (intersection of configured routes +
   the case's `allowed_models`) so the manifest can advertise exactly what the
   agent may use.
3. Emits the **base URLs** the guest's OpenAI client should use:
   - LM Studio routes → the host's LM Studio URL (e.g. `http://10.0.75.1:1234/v1`),
     reachable directly over the vSwitch. **No key, no secret swap.**
   - Cloud routes → the cloud provider's real URL (e.g. `https://api.openai.com/v1`),
     which the guest reaches **through the MITM proxy**. The proxy does the
     dummy→real secret swap (Q6, same `Rewriter` as everywhere). **No separate
     inference service touches the secret.**

This is the **S4 "local networking" choice**, resolving the earlier "OR": the
guest is configured with the right base URLs at manifest-build time and calls
them directly. The proxy handles all secret-bearing traffic uniformly.

## Sequence coverage

- **7.3 (cloud, via proxy):** guest OpenAI client → proxy → cloud. Secret swap
  happens in the proxy. *This crate* only contributes the route config + the
  allowed-models list fed into the manifest.
- **7.3.A (LM Studio, direct):** guest OpenAI client → LM Studio over vSwitch.
  No proxy, no key. *This crate* contributes the LM Studio base URL + which
  models point there.

## Dependencies

- `qwanban-proto` (the `InferenceConfig` / `InferenceRoute` / `RouteTarget` types).
- Read by `qwanban-core` (manifest builder) and `qwanban-broker` (case allowlist).
- Cloud secrets are owned by `qwanban-vault` + `qwanban-proxy` — **not** this crate.

## Routing config (`qwanban.toml`)

```toml
[inference]
lmstudio_url = "http://10.0.75.1:1234/v1"   # host LM Studio, reached over the vSwitch

[[inference.route]]
model = "qwen2.5-coder-32b"          # served by LM Studio (fixed set)
target = "lmstudio"

[[inference.route]]
model = "gpt-4o"                     # cloud — reached via the proxy
target = "cloud"
base_url = "https://api.openai.com/v1"
# NOTE: no `secret` field here. The proxy's search→replace table (secrets.toml)
# maps the case's dummy → the real key. This crate doesn't touch secrets.
```

## What the library exports

```rust
pub struct RouteResolver { /* from InferenceConfig */ }
impl RouteResolver {
    /// Intersection of configured routes + the case's allowed_models.
    pub fn allowed_models(&self, case_allowed: &[String]) -> Vec<String>;
    /// Resolve a model a guest wants to call → its route (LM Studio direct, or
    /// cloud-via-proxy). Returns the base_url the guest should point at.
    pub fn resolve(&self, model: &str, case_allowed: &[String]) -> QwanResult<ResolvedRoute>;
}

pub struct ResolvedRoute {
    pub model: String,
    pub target: RouteTarget,         // Lmstudio | Cloud
    pub base_url: String,            // LM Studio URL for Lmstudio; cloud URL for Cloud
}
```

The manifest builder uses `RouteResolver::allowed_models` to populate the
manifest's `allowed_models` list, and the guest's launch env sets
`OPENAI_BASE_URL` to the LM Studio URL for local models (cloud models go through
the proxy's `https_proxy` env, so the guest's client uses the real cloud URL and
the proxy intercepts/swaps).

## Why no server

- **LM Studio** is already an OpenAI-compatible server on the host; the guest
  calls it directly. Wrapping it in another server adds a hop for nothing.
- **Cloud** traffic is already MITM'd by the proxy, which already owns the
  secret-swap + audit. A second service touching secrets is a redundant trust
  surface and a place for bugs.
- One secret-handling path (the proxy) is simpler and safer than two.

## Testing

- **Unit:** `allowed_models` intersection; `resolve` rejects disallowed models;
  `resolve` returns LM Studio URL for local routes and cloud URL for cloud
  routes (the route config carries no secret — that's the proxy's job).
- No integration server to test; the cloud path's secret swap is tested in
  `qwanban-proxy`, and the LM Studio path is just a direct HTTP call (tested
  end-to-end later in the gated integration harness).

## Open items

- Embeddings/other endpoints scope for v1 (LM Studio already serves `/v1/embeddings`).
- Per-case token budgets: enforce in the proxy audit layer or in broker usage
  records — not here.
