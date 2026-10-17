# Deploy Friendzone and Control Tower on GCE

Run both services on one trusted Ubuntu VM with an attached `control-tower` service account. Keep both management UIs on loopback, reach them through IAP + SSH, and put disposable Windows workers in a separate subnet. Only the controller subnet gets Cloud NAT. Workers get no external IP and no service account.

This playbook creates billable resources. Run the cloud commands yourself from **Cloud Shell or another trusted administrator shell**, not a worker. No Windows ADC setup or service-account key is needed. The operator still needs ordinary `gcloud` login for SSH/IAP.

## Release and readiness

Use these public, pinned source revisions:

| Repository | Revision | What it provides |
| --- | --- | --- |
| `dominiccooney/qwanban`, branch `main` | `b1c070d93531f3874337124851793380bb632471` | Control Tower, official GCE SDK/ADC, worker profiles, callback derivation, ledger recovery; qbt observatory protocol |
| `dominiccooney/friendzone`, branch `master` | `ce63da16c001d9cfb15df843b5379c3199ef8807` | Atomic IP ownership transfer, instance-local metadata protection, unattended Windows CA setup |

Control Tower is included on qwanban `main`. Clones below need no GitHub credentials; check out the pinned application revisions rather than relying on moving branch heads. The playbook itself may be newer than the pinned application revision.

Local validation: 29 Control Tower Rust tests, its packaged Rust/static-Svelte browser test, Svelte checks, Clippy, and ten focused Friendzone integration/bootstrap/contract tests passed on Windows. The fenced shell commands pass Bash/PowerShell syntax checks, and the generated Control Tower YAML parses through the real CLI. **A Linux build, systemd/nftables execution, Windows image boot, real GCE lifecycle and network-isolation checks still need the acceptance run below.** The full qbt suite did not finish locally. Passing local fixtures is not evidence that cloud permissions or firewalls work.

Before starting, have:

- A dedicated automation project with billing enabled and enough CPU/disk quota. Keep production VMs out: the controller role can delete instances in this project, not only names beginning `ct-`.
- Administrator access to enable APIs, create IAM roles/service accounts, networking and the controller VM. Give the runtime account none of those administrative powers.
- A self-contained Windows image built for GCE, with the Google guest environment, qbt, your compatible Cline build, browser, zstd packer and task launcher already installed. This deployment does not download/install the worker agent at boot.
- An image launcher that reads Control Tower metadata, retries Friendzone approval, runs setup as the actual agent account, reports failure/completion and uploads results before completion. See [the image contract](README.md#image-contract).
- A decision about Windows activation. Direct activation is outside the worker egress allowlist below. Validate that the image works under this policy before running real jobs; do not add worker NAT to fix a boot problem.

## 1. Create identity and networking — administrator shell

Use Bash in Cloud Shell. Choose non-overlapping CIDRs and a zone in the image's supported region. Set real project, operator and image values before pasting the remaining commands:

```bash
set -euo pipefail
export PROJECT='your-automation-project'
export OPERATOR_EMAIL='you@example.com'
export ZONE='us-central1-a'
export REGION="${ZONE%-*}"
export NETWORK='control-tower'
export CONTROLLER_SUBNET='control-tower-host'
export WORKER_SUBNET='control-tower-workers'
export CONTROLLER_CIDR='10.77.0.0/24'
export WORKER_CIDR='10.77.16.0/24'
export CONTROLLER_IP='10.77.0.2'
export CONTROLLER='control-tower'
export WINDOWS_IMAGE="projects/$PROJECT/global/images/your-windows-image"
export WINDOWS_MACHINE='n2-standard-4'
export WINDOWS_DISK_GB='80' # Must be at least the source image's disk size.
export OBSERVATORY_PORT='5678'
export SA="control-tower@$PROJECT.iam.gserviceaccount.com"

gcloud config set project "$PROJECT"
gcloud services enable compute.googleapis.com iam.googleapis.com iap.googleapis.com oslogin.googleapis.com
gcloud iam service-accounts create control-tower --display-name='Control Tower'

PERMISSIONS='compute.instances.create,compute.instances.get,compute.instances.delete,compute.instances.setMetadata,compute.instances.setTags,compute.instances.setLabels,compute.disks.create,compute.images.useReadOnly,compute.subnetworks.use,compute.zoneOperations.get'
gcloud iam roles create controlTowerWorkerManager --project="$PROJECT" \
  --title='Control Tower Worker Manager' --stage=GA --permissions="$PERMISSIONS"
gcloud projects add-iam-policy-binding "$PROJECT" \
  --member="serviceAccount:$SA" \
  --role="projects/$PROJECT/roles/controlTowerWorkerManager" --condition=None
gcloud projects add-iam-policy-binding "$PROJECT" \
  --member="serviceAccount:$SA" --role=roles/serviceusage.serviceUsageConsumer --condition=None

gcloud compute networks create "$NETWORK" --subnet-mode=custom
gcloud compute networks subnets create "$CONTROLLER_SUBNET" \
  --network="$NETWORK" --region="$REGION" --range="$CONTROLLER_CIDR"
gcloud compute networks subnets create "$WORKER_SUBNET" \
  --network="$NETWORK" --region="$REGION" --range="$WORKER_CIDR"
gcloud compute addresses create control-tower-internal --region="$REGION" \
  --subnet="$CONTROLLER_SUBNET" --addresses="$CONTROLLER_IP"
gcloud compute routers create control-tower-router --network="$NETWORK" --region="$REGION"
gcloud compute routers nats create control-tower-nat --router=control-tower-router \
  --region="$REGION" --auto-allocate-nat-external-ips \
  --nat-custom-subnet-ip-ranges="$CONTROLLER_SUBNET"
```

Never use `--nat-all-subnet-ip-ranges` here. The workers must remain outside NAT. This single-project recipe does not cover Shared VPC or cross-project service-account attachment. An image from another project needs Image User on that image for the controller account; no broad Editor grant is needed.

Install the firewall rules **before any worker boots**. A fresh custom VPC has no default-network `allow-internal` rule. Lower priority numbers win; check organisation/network firewall policies for conflicting higher-priority allows.

```bash
gcloud compute firewall-rules create ct-iap-ssh --network="$NETWORK" \
  --direction=INGRESS --priority=1000 --action=ALLOW --rules=tcp:22 \
  --source-ranges=35.235.240.0/20 --target-tags=control-tower-host

gcloud compute firewall-rules create ct-worker-services --network="$NETWORK" \
  --direction=INGRESS --priority=1000 --action=ALLOW --rules=tcp:8080,tcp:8082,tcp:8091 \
  --source-ranges="$WORKER_CIDR" --target-tags=control-tower-host

gcloud compute firewall-rules create ct-observatory --network="$NETWORK" \
  --direction=INGRESS --priority=1000 --action=ALLOW --rules="tcp:$OBSERVATORY_PORT" \
  --source-ranges="$CONTROLLER_IP/32" --target-tags=control-tower-worker

gcloud compute firewall-rules create ct-worker-egress --network="$NETWORK" \
  --direction=EGRESS --priority=1000 --action=ALLOW --rules=tcp:8080,tcp:8082,tcp:8091 \
  --destination-ranges="$CONTROLLER_IP/32" --target-tags=control-tower-worker

gcloud compute firewall-rules create ct-worker-deny-egress --network="$NETWORK" \
  --direction=EGRESS --priority=1100 --action=DENY --rules=all \
  --destination-ranges=0.0.0.0/0 --target-tags=control-tower-worker
```

There is no rule for management ports 8081/8090, worker SSH/RDP, or worker-to-worker traffic. Stateful return packets for allowed connections remain allowed. This recipe uses IPv4-only NICs; do not enable IPv6 without equivalent policy.

GCE always allows each VM's own metadata server, which also supplies DNS/DHCP/NTP; VPC deny rules do not block it. Workers therefore still read their own metadata. They must have no service account, and project metadata must contain no secrets. GCE's default anti-spoofing depends on `canIpForward=false`; Control Tower sets that. DNS access can still carry data and Friendzone permits many public destinations: this is practical isolation, not a complete exfiltration sandbox.

Give the operator IAP/OS Login access. These are operator permissions, not runtime permissions. An operator with sudo on the controller is trusted with its secrets and service-account authority.

```bash
gcloud projects add-iam-policy-binding "$PROJECT" \
  --member="user:$OPERATOR_EMAIL" --role=roles/iap.tunnelResourceAccessor --condition=None
gcloud projects add-iam-policy-binding "$PROJECT" \
  --member="user:$OPERATOR_EMAIL" --role=roles/compute.osAdminLogin --condition=None
gcloud projects add-iam-policy-binding "$PROJECT" \
  --member="user:$OPERATOR_EMAIL" --role=roles/compute.viewer --condition=None
gcloud iam service-accounts add-iam-policy-binding "$SA" \
  --member="user:$OPERATOR_EMAIL" --role=roles/iam.serviceAccountUser --condition=None

gcloud compute instances create "$CONTROLLER" --zone="$ZONE" \
  --machine-type=e2-standard-4 --subnet="$CONTROLLER_SUBNET" \
  --private-network-ip="$CONTROLLER_IP" --no-address \
  --service-account="$SA" --scopes=cloud-platform --tags=control-tower-host \
  --image-family=ubuntu-2404-lts-amd64 --image-project=ubuntu-os-cloud \
  --boot-disk-size=80GB --boot-disk-type=pd-balanced --no-boot-disk-auto-delete \
  --metadata=enable-oslogin=TRUE

gcloud compute instances describe "$CONTROLLER" --zone="$ZONE" \
  --format='yaml(networkInterfaces,serviceAccounts,disks)'
gcloud compute routers nats describe control-tower-nat \
  --router=control-tower-router --region="$REGION" --format=yaml
gcloud compute firewall-rules list --filter="network:$NETWORK" --format=yaml
```

Verify: controller has the reserved internal IP, no external address, the dedicated account with `cloud-platform` scope, and NAT selects only the controller subnet. The controller boot disk intentionally survives VM deletion; account for it during teardown. e2-standard-4/80GB are initial build-host choices, not measured runtime requirements.

## 2. Build pinned sources — Ubuntu controller

Connect from the administrator shell:

```bash
gcloud compute ssh "$CONTROLLER" --zone="$ZONE" --tunnel-through-iap
```

The remaining Bash commands in this section run **inside the controller**. Use the ordinary OS Login account for builds, not the runtime service users. A fresh controller should have no local ADC file or `GOOGLE_APPLICATION_CREDENTIALS`; ADC must use the attached account.

```bash
set -euo pipefail
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libssl-dev cmake clang perl \
  git curl ca-certificates unzip xz-utils less python3 nftables

# Download/inspect installer scripts before running them on this trusted build host.
curl -fsS https://sh.rustup.rs -o /tmp/ct-rustup.sh
less /tmp/ct-rustup.sh
sh /tmp/ct-rustup.sh -y --profile minimal --default-toolchain 1.96.0
source "$HOME/.cargo/env"
curl -fsS https://bun.com/install -o /tmp/ct-bun-install.sh
less /tmp/ct-bun-install.sh
bash /tmp/ct-bun-install.sh bun-v1.3.14
export PATH="$HOME/.bun/bin:$PATH"

# Vite/Svelte tooling also launches Node; install the tested Node 24 release.
curl -fsS https://nodejs.org/dist/v24.21.0/SHASUMS256.txt -o /tmp/ct-node-sums
curl -fsS https://nodejs.org/dist/v24.21.0/node-v24.21.0-linux-x64.tar.xz \
  -o /tmp/node-v24.21.0-linux-x64.tar.xz
(cd /tmp; grep ' node-v24.21.0-linux-x64.tar.xz$' ct-node-sums | sha256sum -c -)
sudo tar -xJf /tmp/node-v24.21.0-linux-x64.tar.xz -C /usr/local --strip-components=1

mkdir -p "$HOME/control-tower-build"
cd "$HOME/control-tower-build"
git clone --branch main https://github.com/dominiccooney/qwanban.git
git clone https://github.com/dominiccooney/friendzone.git
git -C qwanban checkout --detach b1c070d93531f3874337124851793380bb632471
git -C friendzone checkout --detach ce63da16c001d9cfb15df843b5379c3199ef8807
test "$(git -C qwanban rev-parse HEAD)" = b1c070d93531f3874337124851793380bb632471
test "$(git -C friendzone rev-parse HEAD)" = ce63da16c001d9cfb15df843b5379c3199ef8807

export NO_PROXY='*'
cargo test --locked --manifest-path qwanban/Cargo.toml -p control-tower
cargo test --locked --manifest-path friendzone/Cargo.toml --test container_restart
cargo build --locked --release --manifest-path qwanban/Cargo.toml -p control-tower
cargo build --locked --release --manifest-path friendzone/Cargo.toml --bin fz
pushd qwanban/observatory
bun install --frozen-lockfile
bun run check
bun run build:tower
popd

sudo install -d -m 0755 /opt/control-tower/bin /opt/control-tower/ui /opt/friendzone/bin
sudo install -m 0755 qwanban/target/release/control-tower /opt/control-tower/bin/control-tower
sudo install -m 0755 friendzone/target/release/fz /opt/friendzone/bin/fz
sudo cp -a qwanban/observatory/build/. /opt/control-tower/ui/
sudo chown -R root:root /opt/control-tower /opt/friendzone
```

Stop if any build/test fails. Do not install a partially validated build. No Node server, compiler or GitHub credential is needed by Control Tower at runtime. Friendzone uses host Git for reviewed branch publication, so retain the `git` package. This builds only the controller crate; qbt's Windows build belongs in the worker image.

## 3. Configure users, data and callbacks — Ubuntu controller

Re-enter the deployment values from section 1 in this shell. They are not automatically carried through SSH:

```bash
export PROJECT='your-automation-project'
export ZONE='us-central1-a'
export REGION="${ZONE%-*}"
export CONTROLLER_IP='10.77.0.2'
export WORKER_SUBNET='control-tower-workers'
export WINDOWS_IMAGE="projects/$PROJECT/global/images/your-windows-image"
export WINDOWS_MACHINE='n2-standard-4'
export WINDOWS_DISK_GB='80'
export OBSERVATORY_PORT='5678'

sudo useradd --system --user-group --home-dir /var/lib/friendzone --shell /usr/sbin/nologin friendzone
sudo useradd --system --user-group --home-dir /var/lib/control-tower --shell /usr/sbin/nologin control-tower
sudo install -d -o friendzone -g friendzone -m 0700 /var/lib/friendzone
sudo install -d -o control-tower -g control-tower -m 0700 /var/lib/control-tower
sudo install -d -o root -g control-tower -m 0750 /etc/control-tower

sudo tee /etc/control-tower/config.yaml >/dev/null <<EOF
operator_addr: 127.0.0.1:8090
worker_addr: $CONTROLLER_IP:8091
data_dir: /var/lib/control-tower
ui_dir: /opt/control-tower/ui
cloud:
  endpoint: https://compute.googleapis.com
  project: $PROJECT
friendzone_url: http://127.0.0.1:8081
worker_profiles:
  windows:
    source_image: $WINDOWS_IMAGE
    zone: $ZONE
    machine_type: $WINDOWS_MACHINE
    subnetwork: projects/$PROJECT/regions/$REGION/subnetworks/$WORKER_SUBNET
    disk_gb: $WINDOWS_DISK_GB
    observatory_port: $OBSERVATORY_PORT
    network_tags: [control-tower-worker]
    metadata:
      friendzone-bootstrap-url: http://$CONTROLLER_IP:8082
limits:
  max_jobs: 2
  max_finished_jobs: 500
  max_artifact_bytes: 1073741824
  artifact_budget_bytes: 10737418240
  upload_timeout_secs: 300
  request_timeout_secs: 30
  job_timeout_secs: 7200
  reconcile_secs: 5
  max_relays: 16
  max_relay_message_bytes: 16777216
EOF
sudo chown root:control-tower /etc/control-tower/config.yaml
sudo chmod 0640 /etc/control-tower/config.yaml
```

`friendzone-bootstrap-url` is a deployment convention for your image launcher, not a key consumed by Control Tower. The launcher must read it explicitly. Control Tower adds its four documented `control-tower-*` keys. Never put real provider credentials in metadata. Leave `max_jobs` small until the first live lifecycle succeeds.

Friendzone uses explicit `--secret-store=file` below. Its provider tokens and CA private key stay in the private data directory. This is plaintext-at-rest application storage on GCE's encrypted disk, not Secret Manager or a login keyring. Protect snapshots/backups and restrict disk/snapshot IAM. Start with a fresh profile; do not copy an OS-keyring marker from your Windows installation.

## 4. Stop proxy traffic reaching controller infrastructure — Ubuntu controller

Friendzone rejects literal metadata/loopback destinations, but it is not a general resolved-DNS SSRF firewall. A worker can ask the proxy to resolve a hostname to a controller/private address. Prevent the **Friendzone Unix user**, including Git subprocesses, from opening those connections in the kernel. Control Tower runs as another user and can reach its loopback API, worker observatories and GCE metadata.

This rule set changes only Friendzone's output, not SSH or other users. Reply packets for accepted worker/operator connections remain allowed. DNS is allowed only to Ubuntu's local stub and GCE's resolver. IPv6 output from Friendzone is denied; this deployment uses IPv4. Private upstream MCP/collector services are intentionally unsupported by this policy.

```bash
FZ_UID=$(id -u friendzone)
sudo tee /etc/control-tower/friendzone-guard.nft >/dev/null <<EOF
table inet control_tower_guard {
  chain output {
    type filter hook output priority filter; policy accept;
    meta skuid $FZ_UID ct direction reply accept
    meta skuid $FZ_UID ip daddr { 127.0.0.53, 169.254.169.254 } udp dport 53 accept
    meta skuid $FZ_UID ip daddr { 127.0.0.53, 169.254.169.254 } tcp dport 53 accept
    meta skuid $FZ_UID fib daddr type local counter reject
    meta skuid $FZ_UID ip daddr { 0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8, 169.254.0.0/16, 172.16.0.0/12, 192.0.0.0/24, 192.168.0.0/16, 198.18.0.0/15, 224.0.0.0/4, 240.0.0.0/4 } counter reject
    meta skuid $FZ_UID meta nfproto ipv6 counter reject
  }
}
EOF

sudo install -d -m 0755 /usr/local/libexec
sudo tee /usr/local/libexec/control-tower-load-guard >/dev/null <<'EOF'
#!/bin/sh
set -eu
file=$(mktemp /run/control-tower-guard.XXXXXX)
trap 'rm -f "$file"' EXIT HUP INT TERM
if /usr/sbin/nft list table inet control_tower_guard >/dev/null 2>&1; then
  printf '%s\n' 'delete table inet control_tower_guard' >"$file"
fi
cat /etc/control-tower/friendzone-guard.nft >>"$file"
/usr/sbin/nft -c -f "$file"
/usr/sbin/nft -f "$file"
EOF
sudo chmod 0755 /usr/local/libexec/control-tower-load-guard

sudo tee /etc/systemd/system/control-tower-guard.service >/dev/null <<'EOF'
[Unit]
Description=Restrict Friendzone output to public upstreams
After=network.target
Before=friendzone.service

[Service]
Type=oneshot
ExecStart=/usr/local/libexec/control-tower-load-guard
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
EOF
```

The loader checks syntax and replaces only its own table in one nftables batch. It does not flush an existing host firewall. Keep these files root-owned; only trusted administrators may alter them. If other firewall software flushes tables, stop Friendzone and reinstall this guard before admitting workers. The Linux checks in section 6 must pass before adding real provider credentials.

## 5. Install services — Ubuntu controller

Generate Friendzone's service with the chosen private address, and install Control Tower's unit. The direct-networking environment below is a choice for this isolated deployment, not a requirement of current Control Tower. Configure SDK proxy routing to suit your network; the pinned older binary still requires `NO_PROXY=*`.

```bash
sudo tee /etc/systemd/system/friendzone.service >/dev/null <<EOF
[Unit]
Description=Friendzone credential broker
Requires=control-tower-guard.service
After=network-online.target control-tower-guard.service
Wants=network-online.target

[Service]
User=friendzone
Group=friendzone
Environment=HOME=/var/lib/friendzone
Environment=NO_PROXY=*
WorkingDirectory=/var/lib/friendzone
ExecStart=/opt/friendzone/bin/fz broker --secret-store=file --data-dir /var/lib/friendzone --proxy-addr $CONTROLLER_IP:8080 --ui-addr 127.0.0.1:8081 --bootstrap-addr $CONTROLLER_IP:8082
Restart=on-failure
RestartSec=5
KillSignal=SIGINT
TimeoutStopSec=45
UMask=0077
NoNewPrivileges=yes
CapabilityBoundingSet=
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/friendzone
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6

[Install]
WantedBy=multi-user.target
EOF

sudo tee /etc/systemd/system/control-tower.service >/dev/null <<'EOF'
[Unit]
Description=Control Tower worker controller
Wants=network-online.target friendzone.service
After=network-online.target friendzone.service

[Service]
User=control-tower
Group=control-tower
Environment=HOME=/var/lib/control-tower
Environment=NO_PROXY=*
Environment=no_proxy=*
WorkingDirectory=/var/lib/control-tower
ExecStart=/opt/control-tower/bin/control-tower --config /etc/control-tower/config.yaml serve
Restart=on-failure
RestartSec=5
KillSignal=SIGINT
TimeoutStopSec=45
UMask=0077
NoNewPrivileges=yes
CapabilityBoundingSet=
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/control-tower
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6

[Install]
WantedBy=multi-user.target
EOF

sudo systemd-analyze verify /etc/systemd/system/control-tower-guard.service \
  /etc/systemd/system/friendzone.service /etc/systemd/system/control-tower.service
sudo systemctl daemon-reload
sudo systemctl enable --now control-tower-guard.service friendzone.service control-tower.service
sudo systemctl --no-pager --full status friendzone.service control-tower.service
sudo journalctl -u friendzone -u control-tower -n 80 --no-pager
```

Control Tower wants Friendzone started first but does not require it to stay up: VM deletion must continue while broker removal is retrying. No `--allow-all-github` flag is set. Both units receive SIGINT on stop, matching their current shutdown handlers. Do not run guest Friendzone setup on the controller or install the broker CA into the controller's system trust.

## 6. Verify host protection, then add credentials

On the controller, check listeners and negative tests **before running a worker**:

```bash
sudo ss -lntp | grep -E ':(8080|8081|8082|8090|8091)\b'
curl --noproxy '*' -fsS http://127.0.0.1:8081/health
curl --noproxy '*' -fsS http://127.0.0.1:8090/api/jobs >/dev/null
sudo nft list table inet control_tower_guard

# Must fail: direct connection as Friendzone to controller management/metadata.
if sudo -u friendzone curl --noproxy '*' -fsS --max-time 5 http://127.0.0.1:8090/api/jobs; then
  echo 'FAIL: Friendzone can reach Control Tower management'; exit 1
fi
if sudo -u friendzone curl --noproxy '*' -fsS --max-time 5 \
  -H 'Metadata-Flavor: Google' http://169.254.169.254/computeMetadata/v1/instance/service-accounts/default/email; then
  echo 'FAIL: Friendzone can reach host service-account metadata'; exit 1
fi
if sudo -u friendzone curl --noproxy '*' -fsS --max-time 5 \
  --resolve "controller-alias.test:8090:127.0.0.1" -H 'Host: 127.0.0.1:8090' \
  http://controller-alias.test:8090/api/jobs; then
  echo 'FAIL: resolved hostname can reach management'; exit 1
fi

# Must succeed: public upstream and controller-user metadata identity, not tokens.
sudo -u friendzone curl --noproxy '*' -fsS --max-time 15 https://example.com/ >/dev/null
sudo -u control-tower curl --noproxy '*' -fsS -H 'Metadata-Flavor: Google' \
  http://169.254.169.254/computeMetadata/v1/instance/service-accounts/default/email
sudo nft list table inet control_tower_guard
```

Verify 8081/8090 bind only `127.0.0.1`, while 8080/8082/8091 bind only the controller private IP. The alias probe supplies a valid operator Host header: failure must come from networking, not the application's Host guard. Negative probes must show a connection rejection, with nftables evidence, rather than a 403, missing binary or generic service outage. The metadata email must be your dedicated account. Do not print or copy access tokens. `serve` starting is not a GCE permission test; token acquisition/permissions are exercised on the first cloud request.

From **your Windows operator terminal**, use ordinary gcloud login and an SSH tunnel. Do not set up ADC or impersonation on Windows:

```powershell
$Project = 'your-automation-project'
$Zone = 'us-central1-a'
gcloud auth login
gcloud compute ssh control-tower --project=$Project --zone=$Zone --tunnel-through-iap `
  --ssh-flag="-N" `
  --ssh-flag="-L 127.0.0.1:8081:127.0.0.1:8081" `
  --ssh-flag="-L 127.0.0.1:8090:127.0.0.1:8090"
```

Keep the gcloud/PuTTY session running. Open `http://127.0.0.1:8081` for Friendzone and `http://127.0.0.1:8090/tower` for Control Tower. Check PuTTY's event log and the local listening sockets if a forward fails; the command intentionally avoids OpenSSH-only `-o` flags. Don't expose the tunnel with a wildcard bind. IAP is for operator SSH, not worker callbacks or public UI hosting.

In Friendzone Settings, create the required escrow entries. For the first smoke test, one Anthropic entry is the simplest path; use a limited real key and the generated fake, never a real credential in the image. If using Cline OAuth, add the Cline entry with no static key and complete **Sign in with Cline** from the operator browser. Rerun guest setup after configuring credentials. MCP forwards require their own guest/tool permissions. New guests have Basic Cline API access; Cloud/Hub work needs Full and a compatible proxy-aware Cline build, not merely an inference credential.

## 7. Confirm the Windows image contract

The image launcher must do these steps; the playbook does not invent a second agent runner:

1. Read `control-tower-job-id`, `control-tower-worker-url`, `control-tower-prompt`, `control-tower-setup-script`, and the configured `friendzone-bootstrap-url` from its **own** metadata server using `Metadata-Flavor: Google`. Use direct requests, not the proxy. Decode scripts as UTF-8.
2. Derive the Friendzone name as `ct-` + the job UUID without hyphens. Retry the broker's `/bootstrap/hello?container=...` until the response says `approved: true`; a 200 or a downloaded setup script alone is not approval.
3. As LocalSystem/elevated provisioning, download `/bootstrap/windows-ca.ps1` and run it to install the public CA into `LocalMachine\Root`, with a download-success check. Initial HTTP trust relies on the protected VPC/controller, not cryptographic server authentication.
4. As the account running Cline, download `/bootstrap/setup?shell=powershell&container=...`, run it, then explicitly dot-source `%APPDATA%\friendzone\friendzone-proxy-env.ps1` in the task launcher. A scheduled task using `-NoProfile` will not load PowerShell profile hooks automatically. Do not set worker `NO_PROXY=*`; it would bypass the credential proxy. Bypass only its own loopback/metadata and direct controller callback/bootstrap destinations.
5. Dot-source `control-tower/worker/worker.ps1`, run `Initialize-QwanbanWorkerJob` for the interactive user (`qa`), and dot-source the generated `job-env.ps1`. This isolates `CLINE_SESSION_DATA_DIR`, `CLINE_DB_DATA_DIR`, `CLINE_LOG_PATH`, `CLINE_HOOKS_LOG_PATH`, and `QBT_ARTIFACT_ROOT` under `<RunsRoot>/<job-id>/` with `CLINE_SESSION_BACKEND_MODE=local` while preserving `~/.cline` for Friendzone's plugin and provider settings.
6. Start qbt's agent listener on guest loopback with `--record-journal --artifact-root $env:QBT_ARTIFACT_ROOT`; expose its configured observatory port through Windows Firewall only to the controller private IP. The VPC rule does not open Windows Firewall for you. Start the interactive Cline session only after bootstrap and launcher checks succeed (`CLINE_COMPUTER_USE_PORT` is registered only in interactive TUI mode, not headless `--prompt`/`--yolo`/`--output json`).
7. After the interactive agent exits, call `Complete-QwanbanWorkerJob -JobRoot $env:CONTROL_TOWER_JOB_ROOT -Outcome completed|failed -StopQbt`. It requests `shutdown_backend` on loopback, verifies writer quiescence and `qbt/journal/*/status.json`, builds an immutable `package/results.tar.zst` with `C:\Windows\System32\tar.exe --zstd -cf`, uploads `/jobs/JOB_UUID/artifact` first, and only then POSTs `{"outcome":"completed"}` or `{"outcome":"failed"}` to `/jobs/JOB_UUID/complete`. No NAT/proxy must change the callback's source IP.

If the task launcher runs in a container, use networking that preserves the VM's unique private IP toward the controller. Worker-to-controller NAT through a shared gateway is unsupported. Include transcripts/screenshots in the final packet if you need them after VM deletion; the live qbt journal does not persist on the controller.

## 8. Run one disposable job, then exercise recovery

On the controller, create a prompt file and submit through the same loopback API used by cron:

```bash
printf '%s\n' 'Take a screenshot, report success, upload a small test packet and finish.' \
  | sudo tee /var/lib/control-tower/smoke-prompt.txt >/dev/null
sudo chown control-tower:control-tower /var/lib/control-tower/smoke-prompt.txt
sudo -u control-tower /opt/control-tower/bin/control-tower \
  --config /etc/control-tower/config.yaml submit --worker-profile windows \
  --title 'GCE smoke test' --source smoke --prompt /var/lib/control-tower/smoke-prompt.txt
sudo -u control-tower /opt/control-tower/bin/control-tower \
  --config /etc/control-tower/config.yaml list
```

Record the returned UUID, VM name, IP, GCE ID and boot-disk name. An accepted submission is not a running worker. In the administrator shell, use `gcloud compute instances describe ct-UUID_WITHOUT_HYPHENS --zone="$ZONE" --format='yaml(id,status,networkInterfaces,serviceAccounts,disks)'` to verify private-only networking, no account, one boot disk with `autoDelete: true`, and the expected image/machine type.

Acceptance checks:

1. Watch Provisioning → Registering → Running. Friendzone must show exactly one approved guest pinned to the same private IP. A GCE 403 is a permission/configuration failure, not evidence that the worker is dead. Fix the specific missing permission; do not add Editor.
2. In the SSH-tunnel UI, view a real transcript/screenshot and capture a fresh screenshot. Browser WebSockets must target the tunnel origin, never worker IPs.
3. From the worker, bootstrap/proxy/callback requests must work. Direct attempts to controller ports 22/8081/8090, a second worker, and public Internet must fail. Stop Friendzone temporarily: direct Internet must still fail. Test a proxy request to a hostname resolving to loopback/private/metadata; it must fail too. Use `--max-time` for every negative probe, and inspect firewall/log evidence rather than treating arbitrary connection errors as proof.
4. Before completing, upload a small packet. Verify it downloads intact and remains an opaque attachment. Completion must end in a history row, remove Friendzone's guest, delete the VM **and its boot disk**. Check the recorded disk directly with `gcloud compute disks describe DISK_NAME --zone="$ZONE"`; expected result is not found.
5. Submit another job; restart Control Tower while Provisioning, then again during Cleanup. Verify the same job/VM identity resumes without a duplicate. Restart Friendzone and verify its pin/credentials persist. Cancel one job and verify both resources disappear. Stop a disposable worker from the administrator shell and verify it becomes Lost and is cleaned up.
6. Reboot the controller. Verify both units and the guard start, the same CA/credential/pin state returns, active jobs reconcile, and all negative tests still fail. Only then increase concurrency and admit untrusted prompts.

Keep an administrator/IAP path open throughout these checks. The operator has cloud authority to remove leaked workers if the controller cannot. Review Windows licensing/activation and image shutdown behavior as part of this run; do not silently widen egress.

## 9. Operate, upgrade and recover

- Logs: `sudo journalctl -u control-tower -u friendzone -f`. Local snapshots only: Control Tower keeps jobs/artifacts; Friendzone's request log is in memory. Journald retention is separate from the artifact budget.
- Stop/restart with systemd; active workers are not deleted simply because the controller exits. Preserve `/var/lib/control-tower`. Failed broker removal remains pending even after VM deletion.
- Change profiles by replacing `/etc/control-tower/config.yaml` atomically with the same owner/mode. Invalid reloads retain the last good config. Existing jobs retain their resolved settings; changing a listener or data/UI directory needs a restart.
- Before upgrading, stop both services and take an access-controlled backup of both data directories and configuration. Backups contain provider tokens and the CA key. Never rotate or discard the CA while workers still trust it. A version-2 ledger cannot be reopened by the first Control Tower build.
- On a lost host, restore both directories, the same private callback address, and equivalent service-account permissions. Don't create a new empty ledger beside live workers: it cannot infer the old jobs. Inspect ownership labels and remove orphaned resources explicitly from the administrator shell.
- Check disk free space. The 10 GiB artifact limit is not a whole-disk quota: builds, logs, Friendzone files and backups consume space too.

To tear down, first cancel every active job and confirm each VM/disk is gone. Save any packets you want, stop both services, then delete the controller from the administrator shell. Its boot disk was created with auto-delete **off** and must be retained deliberately or deleted explicitly. Delete the controller-only NAT/router, reserved internal address and firewall/network resources only after no instances use them. Do not delete a shared project or image as a shortcut. VM, disk, Cloud NAT and reserved-resource charges can continue independently.

## References

- [ADC on a resource with an attached service account](https://docs.cloud.google.com/docs/authentication/set-up-adc-attached-service-account)
- [GCE instance creation permissions](https://docs.cloud.google.com/compute/docs/instances/create-start-instance)
- [IAP TCP forwarding and OS Login prerequisites](https://docs.cloud.google.com/iap/docs/using-tcp-forwarding)
- [VPC firewall statefulness and always-allowed metadata](https://docs.cloud.google.com/firewall/docs/firewalls)
- [Cloud NAT subnet selection](https://docs.cloud.google.com/nat/docs/set-up-manage-network-address-translation)
- [nftables socket UID, connection direction and route expressions](https://netfilter.org/projects/nftables/manpage.html)
- [Friendzone guest bootstrap](https://github.com/dominiccooney/friendzone/blob/ce63da16c001d9cfb15df843b5379c3199ef8807/GUEST-BOOTSTRAP.md) and [isolation limitations](https://github.com/dominiccooney/friendzone/blob/ce63da16c001d9cfb15df843b5379c3199ef8807/NETWORK-ISOLATION.md)