# Component: Inference Router (`qwanban-inference`)

> Owns the OpenAI-compatible inference endpoint guests call, the model
> allowlist, and routing to LM Studio / cloud with real keys injected host-side.
> Read [`README.md`](README.md) §S1–S8. Implements design.md §7, 7.3.A.

## Purpose & scope

A host HTTP service presenting an **OpenAI-compatible API** (`/v1/chat/completions`,
`/v1/models`, optionally `/v1/embeddings`) to guests using a **dummy** key. It
authenticates the case, enforces the per-case model allowlist, routes to the host
**LM Studio** (fixed models) or a **cloud** provider (real key from vault), and
streams responses back. This is the **preferred** inference path (7.3.A); the
proxy path (7.3.B) is a fallback owned by mitm-proxy.

## Sequence coverage

Owns: **7.3.A1–7.3.A9** end to end, and the usage record into broker (7.3.A9).

## Dependencies

- `broker` case registry (validate `case_id`, get `allowed_models`) + audit/usage
  sink.
- `vault` (real keys for cloud routes) — same `Vault` trait as mitm-proxy.
- Upstreams: LM Studio (`http://127.0.0.1:1234/v1`, OpenAI-compatible) and cloud
  providers.

## Endpoint & auth

- Binds on `qwan-internal` (e.g. `https://10.0.75.1:7444`), TLS with the broker/
  host cert (guest pins via manifest, same model as broker).
- **Case binding (S4):** guest sends `Authorization: Bearer DUMMY` +
  `x-qwan-case-id: case_…` + `x-qwan-case-token: …`. The router verifies the
  token↔case binding via the broker case registry. (Chosen binding: the dummy
  key is a fixed literal; the **case_token** in the header is what authenticates —
  resolves the S4 "OR" choice.)
- Invalid/closed case ⇒ `401`. Unknown/forbidden model ⇒ `403 model_not_allowed`
  (7.3.A3).

## Routing

`qwanban.toml`:

```toml
[inference]
lmstudio_url = "http://127.0.0.1:1234/v1"

[[inference.route]]
model = "qwen2.5-coder-32b"          # served by LM Studio (fixed set)
target = "lmstudio"

[[inference.route]]
model = "gpt-4o"                     # cloud
target = "cloud"
base_url = "https://api.openai.com/v1"
secret = "openai_key"               # named in secrets.toml; resolved via vault, injected host-side
```

- **Secrets use the same search→replace model as the proxy** (Q6). The guest's
  OpenAI client sends its dummy (`Bearer <dummy>`) where `<dummy>` is the unique,
  secret-shaped string assigned to this case. The router **swaps the dummy bytes
  for the real secret bytes** wherever they appear in the request (here, in the
  `Authorization` header) — no header-format logic. LM Studio needs no key.
  Dummies + real secrets live in the shared `secrets.toml`; both router and proxy
  hot-reload via `Vault::subscribe()` — rotating a key or adding a dummy takes
  effect on the next request with no restart.
- `GET /v1/models` returns the **intersection** of configured routes and the
  case's `allowed_models` (so the agent only sees what it may use).
- On a chat request: check model ∈ allowed_models (7.3.A2) → resolve route
  (7.3.A4) → forward (7.3.A5/6) injecting the real key for cloud; LM Studio needs
  no key.

## Streaming

- Support `stream:true` SSE: proxy upstream chunks straight through (7.3.A7/8)
  with no buffering that would delay tokens. Non-stream requests pass through as
  a single response.
- Propagate upstream errors as OpenAI-style error bodies; map transport failures
  to `502/503` with `QwanError` (S5) in an `x-qwan-error` header for logs.

## Usage & caps

- After each request, emit a **usage record** to the broker
  (`{case_id, model, prompt_tokens, completion_tokens, target, ts}`) for audit
  and future cost/rate caps (7.3.A9). v1 just records; enforcement is later.

## Why a separate service from the proxy

- The proxy is a *transparent* TLS MITM for arbitrary pinned hosts; this is an
  *explicit* first-party API the guest is configured to call (`OPENAI_BASE_URL`
  in the manifest). Keeping them separate means first-party inference doesn't
  depend on TLS interception, and model-allowlist logic lives in one obvious
  place. Both share `vault`.

## Interfaces (exported)

```rust
pub struct InferenceServer { /* run(addr, routes, case_registry, vault, usage_sink) */ }
// consumes:
pub trait CaseRegistry { fn validate(&self, case_id:&str, token:&str) -> Option<CaseView>; }
// CaseView { allowed_models: Vec<String>, .. }  (from broker)
pub trait Vault { fn secret(&self, name:&str) -> Option<SecretString>; }  // shared w/ proxy
```

## Testing

- **Unit:** model allowlist enforcement; `/v1/models` intersection; route
  resolution; header/case binding.
- **Integration:** fake OpenAI-compatible upstream; assert real key injected for
  cloud, none for LM Studio; SSE streaming passes chunks promptly; disallowed
  model → 403; closed case → 401.
- **Usage:** assert a usage record reaches the broker per request.

## Open items

- Embeddings/other endpoints scope for v1.
- Whether to also enforce a per-case token-budget here vs. purely in audit.
