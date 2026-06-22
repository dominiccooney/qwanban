# Component: TCP Stub Loader (`qwan-stub`, baked into base images)

> The single canonical mechanism for getting the per-case qwan agent + files into
> a guest and launching it — over **plain TCP on the private vSwitch**, with
> **no SSH**. Read [`README.md`](README.md) §S1–S8. Resolves design.md §15.3.
> Implements §5.7 (push, don't rebuild).

## Purpose & scope

`qwan-stub` is a **tiny, stable, rarely-changing** executable the *maintainer*
bakes into every base image (Windows and Linux). It is the only thing that must
pre-exist in the image for bootstrap. At boot it listens on a TCP port and
lets the host:

1. **push** the current `qwan-guest` agent binary (+ any `manifest.agent.files`),
2. **write** the manifest, case token, and proxy CA into the guest,
3. **launch** a command (the qwan agent),
4. relay **stdout/stderr/exit** back to the host for the bootstrap phase.

It is deliberately minimal so it almost never needs to change — the frequently
revved code is `qwan-guest`, which is *pushed* per case (§5.7), never baked.

> **Decision (Q3): TCP stub loader only.** We do **not** set up SSH/sshd in
> images (especially painful on Windows). One mechanism, both OSes, using plain
> TCP over the private vSwitch.

## Sequence coverage

Owns the `guest` side of **7.1.12–7.1.16** (bootstrap listener ready, push_agent,
write_files, launch_agent) and the `agent_pushed` ack (7.1.14 / 7.1.E3). The host
side of that protocol is owned by agent-lifecycle; the TCP *transport* by
hyperv-driver.

## Dependencies

- Host: hyperv-driver (`TcpStream` dialer), agent-lifecycle (the bootstrap
  protocol it speaks).
- Guest: OS TCP support. No other runtime deps — a
  single static binary with no dynamic libraries where possible.

## Lifecycle inside the guest

```
boot ─► OS autostart runs qwan-stub ─► bind TCP(port=STUB) on guest vSwitch IP
     ─► accept ONE host connection (authenticated, see Security)
     ─► serve bootstrap protocol (PUSH_AGENT / WRITE_FILE / LAUNCH / STREAM)
     ─► on LAUNCH: spawn child (qwan-guest), relay stdio/exit
     ─► remain available as a fallback control path until case teardown
```

- **Autostart:** Windows = a Scheduled Task / service set to run at logon of the
  auto-login interactive user (so the later SUT/agent has a desktop session, per
  input-injection). Linux = a systemd unit (or init script) started early.
- **Single-shot accept** then authenticated session; reject additional dialers.

## Bootstrap protocol (over TCP)

Length-prefixed frames (owned jointly with agent-lifecycle; this doc is the guest
implementer):

```
HELLO        { stub_version, os, arch }                 -> host validates
AUTH         { case_bootstrap_secret }                  -> ACK | reject+close
PUSH_AGENT   { sha256, len } <bytes>                    -> ACK{ok|hash_mismatch}
WRITE_FILE   { path, mode, len } <bytes>                -> ACK   # manifest, token, CA, agent.files
LAUNCH       { argv|command, shell, cwd, env[] }        -> ACK{pid}
STREAM       (server->host) { fd, bytes } ...           # stdout/stderr relay
EXIT         (server->host) { code }                    # child exit during bootstrap
```

- `PUSH_AGENT` writes the binary to a known path and **verifies sha256** before
  ACK (matches 7.1.14 / retry on `hash_mismatch` 7.1.E3).
- After `LAUNCH` of `qwan-guest`, the agent takes over (registers with broker over
  the vSwitch, 7.2). The stub stays alive as a **fallback control channel** (e.g.
  to deliver stop/kill if the network path is wedged).

## Per-OS TCP details

- **Windows guest:** plain TCP via Winsock. Bind the well-known STUB port on the
  guest's vSwitch IP. No special socket family, no service GUID registration.
- **Linux guest:** plain TCP. Bind the well-known port on the guest's vSwitch IP.
  Standard in any modern kernel.
- The host dials the guest IP + port (hyperv-driver `open_stream`).

## Security

- **No secrets baked in.** The stub ships with nothing sensitive. The host
  authenticates the session with a per-case `case_bootstrap_secret` the host
  delivers out-of-band at start — distinct from the broker `case_token`.
- The stub only accepts the **host** (TCP on the private vSwitch is host↔guest
  only; not reachable from other VMs or external networks).
- Guest is untrusted anyway (fake keys, §13), so the threat model is modest:
  prevent a *different* host process from hijacking the channel, and verify
  pushed-binary integrity (sha256).

## Versioning

- `HELLO.stub_version` is checked against what the host expects. Because the stub
  is stable, mismatches are rare; on a breaking change the **image must be
  rebuilt** — the one case where §5.7's "no rebuild" doesn't apply, by design,
  since the stub changes ~never. `qwan-guest` is what changes often and is pushed,
  not baked.

## Interfaces

- Guest binary `qwan-stub` (per OS/arch), plus the **image-build requirements**
  the maintainer must satisfy: autostart entry + TCP stub port + proxy CA trust
  + auto-login interactive session. Documented as
  the **base-image contract** (future `base-image.md`).
- Host: speaks the bootstrap protocol via hyperv-driver's `GuestStream`.

## Testing

- **Unit (no Hyper-V):** the bootstrap protocol codec (frames, length prefixes,
  sha256 verify, error paths) over an in-memory duplex stream — fully runnable in
  the dev VM.
- **Unit:** `LAUNCH` child spawn + stdio relay + exit propagation against a fake
  child.
- **Integration (gated, real host):** boot a tiny image with `qwan-stub`, push a
  dummy binary, `LAUNCH echo`, assert relayed stdout + exit code; assert second
  dialer rejected; assert hash-mismatch path.

## Open items

- Exact `case_bootstrap_secret` delivery (initial host-initiated TCP
  handshake vs. a value injected via VM firmware/config).
- Whether to fold the stub's fallback control channel into the broker's directive
  set or keep it bootstrap-only.
- A `base-image.md` documenting the full maintainer image contract.