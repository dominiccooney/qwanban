# Control Tower

Control Tower creates GCE computer-use workers, registers their private IPs with Friendzone, relays qbt's observatory to an operator over an SSH tunnel, and deletes workers after completion, cancellation, timeout or loss. A private on-disk ledger resumes unfinished work after restart. Only one controller may own a data directory.

## Quick start

For a live GCE controller running Friendzone and Control Tower together, follow [the GCE deployment playbook](GCE-PLAYBOOK.md). It pins pushed source revisions and covers attached credentials, networking, headless services and acceptance checks.

From the repository root, build the binary and static Svelte UI:

```sh
cargo build --release -p control-tower
cd observatory
bun install --frozen-lockfile
bun run build:tower
cd ..
```

Copy `control-tower/config.example.yaml` to your deployment directory and edit the listener addresses/ports, project, worker profiles, Friendzone URL and UI/data paths. A worker profile selects the image, zone, machine type, subnet, disk size, network tags and extra metadata. Point `ui_dir` at the built `observatory/build` directory. Paths resolve relative to the YAML file, not the working directory. Keep configuration and data inaccessible to workers. On Windows, set private filesystem ACLs yourself; Unix data directories/files use owner-only permissions.

`worker_addr` is **Control Tower's** worker-facing interface, not a VM address. Control Tower derives `http://<worker_addr>` for callback metadata, so the bind and advertised port cannot disagree. Use an interface IP reachable directly from workers; hostname advertisements and reverse proxies are unsupported.

GCE uses Google's official Rust client and Application Default Credentials (ADC). On GCE, use the controller host's service account; never attach it to workers. For local development, run `gcloud auth application-default login` once. `GOOGLE_APPLICATION_CREDENTIALS` can select a credential file. Credential selection takes effect at server startup; the SDK caches and refreshes access tokens. `cloud.endpoint` is an origin such as `https://compute.googleapis.com`; the SDK supplies `/compute/v1`. Do not include that path in the endpoint.

```sh
control-tower --config /etc/control-tower/config.yaml serve
```

The Google SDK owns API and authentication networking. Configure `HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY` for your deployment; Control Tower does not require `NO_PROXY=*` or override SDK routing. Restart the server after changing its proxy environment or credential selection. Friendzone management and operator CLI calls use direct loopback HTTP.

Forward the configured operator port, then open `/tower` (the root redirects there):

```sh
ssh -N -L 8090:127.0.0.1:8090 operator@controller-host
```

Use the loopback REST API or the CLI for cron:

```sh
control-tower --config /etc/control-tower/config.yaml submit \
  --worker-profile windows --title "QA checkout" --source cron \
  --prompt /srv/jobs/prompt.txt --setup-script /srv/jobs/setup.ps1
control-tower --config /etc/control-tower/config.yaml list
control-tower --config /etc/control-tower/config.yaml cancel JOB_UUID
control-tower --config /etc/control-tower/config.yaml retry JOB_UUID
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

The Friendzone guest name is `ct-` followed by the UUID without hyphens. Configure its proxy/bootstrap URLs through worker-profile metadata or the image's existing bootstrap configuration. No real credentials enter metadata. Each metadata value must fit GCE's 256 KiB limit; all keys/values together must be below 512 KiB.

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

For Windows workers, [`control-tower/worker/worker.ps1`](worker/worker.ps1) provides PowerShell 7 helpers for the interactive desktop account (`qa`):

- `Read-QwanbanWorkerMetadata` and `Wait-QwanbanFriendzoneApproval` read the `control-tower-*` and `friendzone-bootstrap-url` metadata attributes directly (`--noproxy '*'`) and poll `/bootstrap/hello` until `approved: true`.
- `Initialize-QwanbanWorkerJob` creates an isolated `<RunsRoot>/<job-id>/` directory (`run.json`, `prompt.txt`, `setup.ps1`, `job-env.ps1`, `cline/`, `qbt/`, `output/`, `package/`). Dot-sourcing `job-env.ps1` sets `CLINE_SESSION_BACKEND_MODE=local`, job-scoped `CLINE_SESSION_DATA_DIR`, `CLINE_DB_DATA_DIR`, `CLINE_LOG_PATH`, `CLINE_HOOKS_LOG_PATH`, `CLINE_COMPUTER_USE_PORT`, and a `CLINE_COMPUTER_USE_BACKEND_COMMAND` that starts `qbt serve` against `<job-root>/qbt` while leaving `~/.cline` (`CLINE_DIR`) intact for Friendzone's plugin and provider settings.
- `Complete-QwanbanWorkerJob` stops `qbt` cleanly (`shutdown_backend`), verifies all tracked writer PIDs have exited and `qbt/journal/events.jsonl` is flushed and valid, builds `package/results.tar.zst` once via `C:\Windows\System32\tar.exe --zstd -cf`, uploads `/jobs/{id}/artifact` (`201`/`204`), and only then reports `/jobs/{id}/complete` (`202`).

Note that on the `dpc/computer-use` Cline branch, `CLINE_COMPUTER_USE_PORT` registers the `computer` tool only in interactive TUI mode (`cline` without headless `--prompt`/`--yolo`/`--output json` flags).

## Network and cloud prerequisites

The operator listener must be loopback; the worker listener must bind a specific interface on a different port. The worker router exposes only upload/completion. It never exposes submission, listing, cancellation, downloads or relays. Forwarded-IP headers are ignored. Browser Host/Origin checks guard the unauthenticated operator interface against cross-origin requests and DNS rebinding; local processes remain trusted.

Enforce outside the guest:

- Workers may reach Friendzone's proxy/bootstrap and Control Tower's worker port, not either management port or another worker. Prevent source-IP spoofing.
- The controller may reach worker observatory ports. Operator browsers do not need worker connectivity.
- Exclude the worker subnet from Cloud NAT/shared forwarding. VMs have no external IP, no IP forwarding and no attached service account. No external IP alone is **not** an egress firewall.
- Check that Friendzone cannot relay to Control Tower management through hostname aliases or another host service. Its existing protections are not a general resolved-DNS LAN firewall; use host firewall/process confinement where necessary.
- Audit project-level startup scripts/metadata and guest-installed software. Do not place controller credentials in the worker image or metadata.

Give only the **controller** the GCE permissions needed to create/get/delete instances, get zonal operations, create/use boot disks and read the configured images/subnetwork. Prefer a scoped custom IAM role. Friendzone/CLI HTTP calls disable environment proxies and redirects. Google's SDK uses its own transport and ADC; its redirect behavior is the SDK default, so trust and protect endpoint/credential configuration. GCE failures/timeouts remain pending or visible errors; they do not silently remove a worker. SDK RPC retries and automatic operation polling are disabled: Control Tower persists operation names and retries from the ledger, with an outer deadline covering authentication and the RPC.

## Reload and recovery

The service checks YAML contents each second, including atomic file replacements. Invalid YAML, unknown fields and invalid values retain the last good snapshot and show an error in the UI. Listener addresses and data/UI directories require restart; changing one rejects the entire reload. Worker profiles, endpoint and provisioning settings apply to **new submissions**; existing jobs retain their configuration snapshot for recovery and cleanup. Limits apply to the next request/maintenance pass; an upload/relay already admitted retains its limits.

To upgrade the first configuration format, rename `images` to `worker_profiles`, replace `cloud.api_url` with the origin-only `cloud.endpoint`, and remove `worker_url` and `cloud.auth`. The CLI flag/API submission field are now `--worker-profile`/`worker_profile`. Version-1 and version-2 ledgers migrate atomically to version 3, preserving identities, operation names, resolved profiles and existing callback URLs. Old per-job token sources are replaced by host ADC: configure equivalent permissions **before restarting**. Older binaries cannot read version 3; back up the stopped service's data directory before upgrading if rollback is needed. YAML is not rewritten automatically; old fields are rejected.

Provisioning settings are persisted, so keep old cloud/Friendzone endpoints available until their jobs finish. Cloud API errors are not evidence of death. Missing or stopped instances become `lost`, enter cleanup, and leave the active list only once removal/deletion finishes. Failed Friendzone removal does not prevent an independent VM deletion attempt. Ctrl-C stops the service without deleting active jobs; restart with the same data directory to resume them. Ledger commit failures stop background reconciliation rather than continue with unrecorded writes.

Insufficient scopes, denied permissions, invalid requests and unsupported responses stop automatic attempts and show `blocked` with the error. Transient network/timeouts, rate limits and service failures back off for 5, 10, 20 and 40 seconds; the fifth consecutive failure blocks the job. A successful reconciliation resets the failure count. These retry states survive restart. After fixing the cause, use **Retry**, the CLI `retry JOB_UUID`, or `POST /api/jobs/JOB_UUID/retry` with `{}` to resume the same job, insertion UUID and pending operation. A blocked job is not finished: it may still own a VM or need cleanup. Cancel requests cleanup without discarding uncertain provisioning; cleanup can itself block and need Retry. Job deadlines still initiate cleanup while monitoring is blocked.

The lifecycle controller is deliberately serialized. Slow cloud calls can delay submissions/completion; the read-only dashboard uses the last committed snapshot. The ledger is intended for a modest single-controller installation, not a distributed scheduler. Transcripts and screenshots remain qbt's live bounded journal and disappear with its VM unless the worker includes them in its packet.

## Tests

```sh
cargo test -p control-tower
pwsh -NoProfile -File control-tower/worker/test-worker.ps1
cd observatory
bunx playwright install chromium
bunx playwright test --config playwright.tower.config.ts
cd ..
```

Integration tests use local HTTP/WebSocket fixtures, not your GCE project. If your environment uses a proxy, bypass `localhost` and `127.0.0.1` for these fixtures. For a deployment smoke test, use a disposable image and enforced network rules:

1. Submit a short task. Verify its private IP is pinned in Friendzone and the dashboard shows its submission, transcript and screenshot. Take a fresh screenshot through the SSH tunnel.
2. Upload a packet and report completion. Verify the packet downloads intact and both the VM **and boot disk** disappear.
3. Repeat, restarting Control Tower during provisioning and cleanup. Verify the same job resumes without creating a second VM.

Live GCE/network-policy validation is a deployment requirement, not covered by local fixtures. Rust 1.91 or newer is required. Local checks have been run on Windows with Rust 1.96 and Node 24; the Unix permission/fsync paths still need a Linux deployment run.