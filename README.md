# qwanban

A Rust tool for controlling **Hyper-V VMs** on a Windows host to run
**QA-type agentic development tasks**.

Two primary use cases:

1. **Scripted QA** — run human-readable (text/Markdown) QA scripts inside an
   ephemeral VM and generate a QA report with reproducible repros. Commit
   attribution ("bisection") is handled by the agent at the prompt level.
2. **Bug repro & fix** — ingest bug reports, reproduce and fix them inside a VM,
   and open a PR with screen-recorded evidence.

Key properties:

- Windows host + Hyper-V; **Windows or Linux guests** (defaults to Linux, agent
  can request OS migration).
- **Host is non-agentic.** The tester is a **Cline** agent (SDK / CLI /
  extension) running inside an untrusted, ephemeral VM, alongside a small **qwan
  agent** that provides breadcrumbs, handoffs, transcript exfil, and a **qwan
  MCP** giving the Cline agent **computer control** of the guest.
- **Continuous screen recording** synced to transcript **breadcrumbs**, with
  agent-generated **clippings** for PRs, viewable in a web report. Video may be
  compressed in the guest or on the host — whichever is easier.
- GPU-less guests make **inference** requests via the host (LM Studio / cloud).
- **Base images** are maintainer-built VHD/VHDX files registered by **file
  path**. The qwan agent is **pushed per-case over TCP** (via a tiny
  baked-in `qwan-stub` loader; no SSH) so it can be revved without rebuilding
  images.
- Guests hold only **dummy keys** (real-looking, unique strings — so the agent can
  juggle multiple tokens and even hide them in a chroot); a host **MITM HTTPS
  proxy** (Rust: `hudsucker` + `rcgen`) pins traffic to trusted hosts and swaps
  the dummy bytes for real ones via a global search→replace table. Real keys live
  in a simple **file on the host**, hot-reloaded. Exfiltration of the fake keys is
  not a concern.
- **Host-configured resource caps** (CPU/RAM/disk/runtime/concurrency).

See [`docs/design.md`](docs/design.md) for the full design.
