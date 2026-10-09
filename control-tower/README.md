# Control Tower

Control Tower creates GCE computer-use workers, registers their private IPs with Friendzone, relays qbt's observatory to an operator over an SSH tunnel, and deletes workers after completion, cancellation, timeout or loss. A private on-disk ledger resumes unfinished work after restart. Only one controller may own a data directory.

## Quick start

From the repository root, build the binary and static Svelte UI:

```sh
cargo build --release -p control-tower
cd observatory
bun install --frozen-lockfile
bun run build:tower
cd ..
```

Copy `control-tower/config.example.yaml` to your deployment directory and edit every deployment value: listener addresses/ports, worker URL, project, zone, image, machine type, subnet, Friendzone URL and UI/data paths. Point `ui_dir` at the built `observatory/build` directory. Paths resolve relative to the YAML file, not the working directory. Keep configuration and data inaccessible to workers. On Windows, set private filesystem ACLs yourself; Unix data directories/files use owner-only permissions.

```sh
control-tower --config /etc/control-tower/config.yaml serve
```

Forward the configured operator port, then open `/tower` (the root redirects there):

```sh
ssh -N -L 8090:127.0.0.1:8090 operator@controller-host
```

Use the loopback REST API or the CLI for cron:

```sh
control-tower --config /etc/control-tower/config.yaml submit \
  --image windows --title "QA checkout" --source cron \
  --prompt /srv/jobs/prompt.txt --setup-script /srv/jobs/setup.ps1
control-tower --config /etc/control-tower/config.yaml list
control-tower --config /etc/control-tower/config.yaml cancel JOB_UUID
```

Submission prints its UUID before making the request. Reuse `--id UUID` with exactly the same submission when retrying an uncertain request. An existing ID returns the existing job, not another VM. Expired IDs cannot be reused.

## Image contract

The image owns boot/task execution, including retrying Friendzone until registration succeeds. Control Tower creates the VM, discovers its private IP, then registers it; it does not gate boot or execute the setup script itself.

The instance receives these custom metadata entries:

| Key | Value |
| --- | --- |
| `control-tower-job-id` | Submission UUID |
| `control-tower-worker-url` | Worker API origin |
| `control-tower-prompt` | UTF-8 prompt, possibly empty |
| `control-tower-setup-script` | UTF-8 setup script, possibly empty |

The Friendzone guest name is `ct-` followed by the UUID without hyphens. Configure its proxy/bootstrap URLs through image-profile metadata or the image's existing bootstrap configuration. No real credentials enter metadata. Each metadata value must fit GCE's 256 KiB limit; all keys/values together must be below 512 KiB.

Run qbt's agent listener on guest loopback and make its observatory port reachable **only from the controller**. Containers must use networking that preserves the VM's unique private source IP; NAT through a shared gateway is unsupported.

Before completion, optionally upload one zstd packet. Contact this endpoint directly, not through Friendzone or another HTTP proxy: source IP is identity.

```sh
curl --fail --noproxy '*' -X POST --data-binary @results.tar.zst \
  "$CONTROL_TOWER_WORKER_URL/jobs/$CONTROL_TOWER_JOB_ID/artifact"
curl --fail --noproxy '*' -X POST -H 'Content-Type: application/json' \
  --data '{"outcome":"completed"}' \
  "$CONTROL_TOWER_WORKER_URL/jobs/$CONTROL_TOWER_JOB_ID/complete"
```

Use `"failed"` instead of `"completed"` for task failure. Retry non-success responses; wait for successful upload before completion. Successful upload is immutable and retrying it returns 204. Completion returns 202 for durable acceptance, **not** completed VM deletion. Workers need not shut themselves down: Control Tower deletes the VM and its auto-delete boot disk. Completion retries are idempotent for the same outcome while history remains.

Packets are streamed with size, storage-budget and time limits, checked for zstd frame magic, and stored without decompressing or extracting. Frame magic is not archive validation. Treat downloaded contents as untrusted. Oldest finished packets expire when storage exceeds the configured budget; history is also bounded by `max_finished_jobs`. Pending uploads reserve capacity and never become downloads. Small retired-ID tombstones remain to prevent identity reuse.

## Network and cloud prerequisites

The operator listener must be loopback; the worker listener must bind a specific interface on a different port. The worker router exposes only upload/completion. It never exposes submission, listing, cancellation, downloads or relays. Forwarded-IP headers are ignored. Browser Host/Origin checks guard the unauthenticated operator interface against cross-origin requests and DNS rebinding; local processes remain trusted.

Enforce outside the guest:

- Workers may reach Friendzone's proxy/bootstrap and Control Tower's worker port, not either management port or another worker. Prevent source-IP spoofing.
- The controller may reach worker observatory ports. Operator browsers do not need worker connectivity.
- Exclude the worker subnet from Cloud NAT/shared forwarding. VMs have no external IP, no IP forwarding and no attached service account. No external IP alone is **not** an egress firewall.
- Check that Friendzone cannot relay to Control Tower management through hostname aliases or another host service. Its existing protections are not a general resolved-DNS LAN firewall; use host firewall/process confinement where necessary.
- Audit project-level startup scripts/metadata and guest-installed software. Do not place controller credentials in the worker image or metadata.

Give only the **controller** the GCE permissions needed to create/get/delete instances, get zonal operations, create/use boot disks and read the configured images/subnetwork. Prefer a scoped custom IAM role. The `metadata` auth mode uses the controller host service account; local `command` auth mode runs the configured executable directly with arguments, no shell. All HTTP clients disable environment proxies and redirects. GCE failures/timeouts remain pending or visible errors; they do not silently remove a worker.

## Reload and recovery

The service checks YAML contents each second, including atomic file replacements. Invalid YAML, unknown fields and invalid values retain the last good snapshot and show an error in the UI. Listener addresses and data/UI directories require restart; changing one rejects the entire reload. Image, endpoint and provisioning settings apply to **new submissions**; existing jobs retain their complete configuration snapshot for recovery and cleanup. Limits apply to the next request/maintenance pass; an upload/relay already admitted retains its limits.

Provisioning settings are persisted, so keep old cloud/Friendzone endpoints available until their jobs finish. Cloud API errors are not evidence of death. Missing or stopped instances become `lost`, enter cleanup, and leave the active list only once removal/deletion finishes. Failed Friendzone removal does not prevent VM deletion; removal continues retrying. Ctrl-C stops the service without deleting active jobs; restart with the same data directory to resume them. Ledger commit failures stop background reconciliation rather than continue with unrecorded writes.

The lifecycle controller is deliberately serialized. Slow cloud calls can delay submissions/completion; the read-only dashboard uses the last committed snapshot. The ledger is intended for a modest single-controller installation, not a distributed scheduler. Transcripts and screenshots remain qbt's live bounded journal and disappear with its VM unless the worker includes them in its packet.

## Tests

```sh
cargo test -p control-tower
cd observatory
bunx playwright install chromium
bunx playwright test --config playwright.tower.config.ts
cd ..
```

Integration tests use local HTTP/WebSocket fixtures, not your GCE project. For a deployment smoke test, use a disposable image and enforced network rules:

1. Submit a short task. Verify its private IP is pinned in Friendzone and the dashboard shows its submission, transcript and screenshot. Take a fresh screenshot through the SSH tunnel.
2. Upload a packet and report completion. Verify the packet downloads intact and both the VM **and boot disk** disappear.
3. Repeat, restarting Control Tower during provisioning and cleanup. Verify the same job resumes without creating a second VM.

Live GCE/network-policy validation is a deployment requirement, not covered by local fixtures. Local checks have been run on Windows with Rust 1.96 and Node 24; the Unix permission/fsync paths still need a Linux deployment run.