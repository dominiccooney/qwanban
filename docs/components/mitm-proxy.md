# Component: MITM HTTPS Proxy (`qwanban-proxy`)

> Owns TLS interception, host pinning, dummy→real key rewrite, audit, and the
> git/PR proxy path. Read [`README.md`](README.md) §S1–S8. Implements design.md
> §8 and the PR path in §10 / 7.8.

## Purpose & scope

A host-run intercepting proxy that all guest HTTPS egress is forced through
(via system `HTTPS_PROXY` + the guest trusting the qwan CA). It:

1. **pins** outbound requests to an allowlist of trusted hosts,
2. **swaps** any known dummy string for its real secret via a global
   search→replace table (no header-format logic, no auto-injection),
3. **audits** every mediated request.

It is the *only* component besides `vault` that reads real secrets. Per design.md
§13, exfil of the fake guest keys is **not** a concern; the proxy's job is to
ensure the **real** keys are added only when forwarding to pinned hosts.

## Sequence coverage

Owns: **7.3.B1–7.3.B8** (third-party HTTPS inference path), **7.8.2–7.8.9** (git/
PR push with token rewrite). Shares the vault with inference-router (7.3.A is
owned there, not here).

## Dependencies

- `vault` (host file `secrets.toml`, S7) for real keys.
- `broker` for the audit sink (`audit(...)`).
- Rust libs: **`hudsucker`** (MITM HTTP/S proxy) + **`rcgen`** (CA + leaf certs),
  rustls. (design.md §8.3 decision.)

## CA & interception

- On first run, generate a **qwan CA** (rcgen): private key stays on host
  (`secrets`/state dir); the **public cert** is shipped into base images at build
  time and its SPKI fingerprint is passed to guests in the manifest
  (`proxy.ca_fpr_sha256`).
- For each intercepted host, mint a leaf cert signed by the qwan CA
  (`hudsucker` `RcgenAuthority`), cache per-host. The guest trusts the CA, so TLS
  terminates transparently.
- The proxy makes a **separate, fully-validated** TLS connection to the real
  upstream (normal root store) — it never weakens upstream verification.

## Request handling pipeline (per request)

```
1. parse CONNECT host:port; record case via source binding (see "case attribution")
2. host pin check: host ∈ allowlist?  no -> 403 Blocked (audit) ; END
3. method/path policy (per-host): allowed?  no -> 403 ; END
4. rewrite (search→replace): scan request headers + URL/query + body for any
   known dummy string; for each hit, replace dummy bytes with the real secret.
   No known dummy found -> leave request untouched (no injection).
5. forward to upstream over validated TLS; stream response back to guest
6. audit: {case_id, host, method, path, status, bytes_up/down, which dummies matched, ts}
```

### Allowlist & rewrite rules (config, owner)

The rewriter is a plain **search → replace** table, applied to bytes of allowed
requests. There is **no header-format logic and no auto-injection.** Key points:

- **Dummies are real-looking and unique, not a `DUMMY` sentinel.** Each dummy is a
  distinct, secret-shaped string the guest carries (e.g. `ghp_qwanDUMMY01aB…`,
  `sk-qwanDUMMY7c…`). This lets the agent **juggle multiple distinct tokens**
  (two GitHub accounts → two distinct dummies), and even **hide** its dummy in a
  chroot — the rewriter doesn't care where the client got it, only that it appears
  in the request.
- **`search → replace` is a substring/byte replacement**, not a header template.
  The guest already emits whatever format the upstream expects
  (`Authorization: Bearer <dummy>`, `Basic base64("x-access-token:<dummy>")`, a
  URL `?token=<dummy>`, a JSON body field, etc.); the rewriter just swaps the
  matched dummy bytes for the real secret bytes. Whatever header/encoding the
  client used is preserved.
- **No known dummy ⇒ pass through unchanged.** A request to an allowlisted host
  that carries none of the known dummies is forwarded verbatim. We never slap
  credentials onto a request that didn't bring its own.

Config lives in **`secrets.toml`** on the host (convenience: one file, hot-reload).
It has two sections:

```toml
# secrets.toml — host file, owned here, hot-reloaded (Q6)

# Real secrets, by name. Values never leave the host.
[real]
github_token   = "ghp_REAL…"
github_token_2 = "ghp_REAL_2…"   # a second GH account the agent juggles
openai_key     = "sk-REAL…"
anthropic_key  = "sk-ant-REAL…"

# The rewrite table: dummy (search) -> real secret name (replace).
# Each dummy MUST be unique and secret-shaped. Only these exact bytes are matched.
[[rewrite]]
search = "ghp_qwanDUMMY01aB…"   # the dummy the guest was given for account 1
replace = "github_token"        # -> resolves to real.github_token

[[rewrite]]
search = "ghp_qwanDUMMY99zZ…"   # distinct dummy for account 2 (juggling)
replace = "github_token_2"

[[rewrite]]
search = "sk-qwanDUMMY7c…"
replace = "openai_key"

[[rewrite]]
search = "sk-ant-qwanDUMMY0x…"
replace = "anthropic_key"
```

The **allowlist of hosts** (what the guest may reach at all) lives in `qwanban.toml`
and is separate from the rewrite table — a host can be allowlisted with no rewrite
attached:

```toml
# qwanban.toml
[[proxy.host]]
host = "api.github.com"          # exact match
allow_methods = ["GET","POST","PATCH","PUT"]
# no rewrite needed — the dummy→real swap is global, host-independent

[[proxy.host]]
host = "github.com"              # git over https
allow_methods = ["GET","POST","PUT","PROPFIND","MKCOL","PATCH","LOCK","UNLOCK"]

[[proxy.host]]
host = "raw.githubusercontent.com"
allow_methods = ["GET"]

[[proxy.host]]
host = "api.anthropic.com"

[[proxy.host]]
host_suffix = "blob.core.windows.net"   # whole domain
allow_methods = ["GET","PUT"]
```

Semantics:

- **The rewrite table is global, not per-host.** Any known dummy found in an
  allowed request is replaced, regardless of which allowed host it's going to. This
  is what makes juggling work: the same dummy string is swapped the same way
  everywhere, and the *format* the client used is preserved.
- **Allowlist vs rewrite are independent.** Allowlist = the security boundary
  (unknown host ⇒ `403 Blocked`). Rewrite = a convenience that only affects bytes
  that already contain a known dummy.
- **No known dummy in an allowed request ⇒ pass through verbatim.** No injection.
- **Host matching:** exact hostname beats `host_suffix`.
- **Where it searches:** the rewriter scans request headers + the request
  URL/query + the (decoded, if transparent) request body for each known dummy.
  Bodies are scanned because some APIs put the token in JSON. (Streaming bodies are
  scanned in a bounded window — see Open items.)

#### How dummies reach the guest

Dummies are generated/assigned per case (or per job, if the same juggling set
should survive an OS migration) by the host and delivered to the guest via the
manifest — **not** baked into the image. The guest then uses them exactly as it
would real tokens (env vars, git credential helpers, `~/.netrc`, config files,
even inside a chroot). Because they're secret-shaped and unique, the agent can
manage multiple at once and keep them private from the SUT if it wants.

### Hot-reload (DECIDED, Q6)

Both `qwanban.toml` (the host allowlist) and `secrets.toml` (the `[real]` secrets
+ the `[[rewrite]]` dummy→secret table) are **watched and hot-reloaded** without
restarting the proxy:

- The proxy holds the snapshot (allowlist + rewrite table + real secrets) behind
  an `ArcSwap<Snapshot>`; the file watcher does a **parse → validate → atomic
  swap**. In-flight requests keep the snapshot they started with; new requests use
  the new snapshot.
- `secrets.toml` changes (rotate a real value, add a new dummy→secret pair,
  add a provider) take effect for the next request — **no restart**.
- Validation on load: every `[[rewrite]].replace` must resolve to a `[real]`
  entry; every dummy `search` must be unique; load failure keeps the *previous*
  snapshot and emits an audit `ConfigError` (never a broken partial config).
- Vault (shared with inference-router) exposes `subscribe()` so inference routes
  reload on the same `secrets.toml` change.

### Case attribution

- The proxy maps an incoming connection to a `case_id` by **source IP** (each
  case VM has a distinct lease on `qwan-internal`) or by a per-case
  `Proxy-Authorization` injected via manifest. This lets audit + (future) per-case
  rate limits work.

## Git/PR path (7.8)

Git over HTTPS and the `gh` API both hit pinned `github.com`/`api.github.com`;
the same global search→replace swaps the dummy token for the real one wherever it
appears (Basic-auth header for git, Bearer header for the API). No special-casing
per host — the dummy bytes are the same string either way. The PR body (with clip
web_urls) is the guest's content; the proxy only swaps the dummy.

## Audit

- Every mediated request emits an audit record to the broker (async, best-effort
  but durable-queued). Record = `{case_id, host, method, path, status,
  bytes_up, bytes_down, key_id (NOT the secret), ts}`.
- This is the hook for future **rate limits / Bayesian abuse detection**
  (design.md §13) — out of scope for v1 but the audit stream is the input.

## Interfaces (exported)

```rust
pub struct ProxyServer { /* run(addr, ca, allowlist_watcher, secrets_watcher, audit_sink) */ }
pub trait AuditSink { async fn record(&self, r: AuditRecord); }   // impl forwards to broker

// Allowlist (from qwanban.toml) — the security boundary, independent of rewrites.
pub struct Allowlist { pub hosts: Vec<HostRule> }
pub struct HostRule {
    pub match: HostMatch,                 // Exact(String) | Suffix(String); exact beats suffix
    pub allow_methods: Vec<Method>,
}

// Rewrite table (from secrets.toml) — dummy -> real. Global, host-independent.
pub struct RewriteTable { pub entries: Vec<RewriteEntry> }
pub struct RewriteEntry {
    pub search: Vec<u8>,                  // the unique, secret-shaped dummy bytes
    pub replace_secret: String,           // name into [real] in secrets.toml
}
// Allowlist + rewrite table + real secrets are held behind an ArcSwap<Snapshot>
// and swapped atomically on hot-reload (Q6). In-flight requests keep their snapshot.
```

Vault interface (shared with inference-router — owns real secret *values* + reload):
```rust
pub trait Vault: Send + Sync {
    /// Current value of a named real secret (resolved from secrets.toml [real]).
    fn secret(&self, name: &str) -> Option<SecretString>;
    /// Subscribe to value/table changes for hot-reload (proxy + inference-router).
    fn subscribe(&self) -> BoxStream<'static, SecretsReloaded>;
    /// Validate that every name referenced by the rewrite table exists; on reload.
    fn validate(&self, table: &RewriteTable) -> Result<()>;
}
```

## Testing

- **Unit:** allowlist match (exact vs suffix, exact-wins); method policy; unknown-
  host deny.
- **Unit (search→replace):** a dummy in a `Bearer` header, in a `Basic` header, in
  a URL `?token=`, and in a JSON body field are each found and swapped for the
  real secret, preserving the surrounding format; **two distinct dummies** in one
  request both swap to their respective real secrets (juggling); a partial/changed
  dummy string is **not** matched (no false positives).
- **Unit (no-injection):** a request to an allowed host carrying **no** known
  dummy is forwarded byte-for-byte unchanged; the real secret is never added.
- **Unit (hot-reload):** edit `secrets.toml` (rotate a real value, add a new
  dummy→secret pair) → next request uses the new table with no restart; an
  in-flight request keeps the old snapshot; a malformed reload keeps the previous
  snapshot and emits `ConfigError`; a reload with an unresolved `replace` name is
  rejected (no partial config).
- **Integration (loopback upstreams):** fake TLS upstream + guest client trusting a
  test CA; assert dummy→real swap and the upstream sees the real secret; blocked
  host returns 403 + audit row; an allowed-but-dummyless request is unchanged.
- **Git path:** `git push` through the proxy to a local git server, dummy token in
  the Basic auth swapped for the real one.
- **Security:** real secret never appears in audit (only which dummy matched);
  upstream cert validation still enforced (MITM of the upstream fails).

## Open items

- Per-case rate limits / quotas (post-v1) — design the audit→limiter feedback.
- Streaming/SSE correctness for inference via 7.3.B (ensure no buffering breaks
  token streaming).
- **Body-scan window for streaming/chunked uploads:** large or streaming request
  bodies can't be fully buffered. Decide the bounded scan window (e.g. first N KB
  + rolling overlap) and whether dummies spanning chunk boundaries are supported.
