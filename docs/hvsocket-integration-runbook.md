# Real hvsocket Integration Test - Runbook

This runbook drives a **real** AF_HYPERV hvsocket bootstrap handshake between
the host and a guest VM running `qwan-bootstrapd` (the production stub loader,
packaged as a daemon). No mocks - real sockets, real file pushes, a real
launched process.

The guest VM in this dev environment is the target. Its VM GUID is:

```
995A044D-0B4C-424A-9E8A-05EFCE117BE5
```

The fixed service GUID the stub listens on:

```
3045196F-2A11-4D65-BCC7-3F9EAB09B7ED
```

## Architecture

```
HOST (your machine)                         GUEST (this dev VM, persistent)
+----------------------+                   +--------------------------+
|  host-harness binary | --AF_HYPERV----->|  qwan-bootstrapd (run)   |
|  connect_hvsocket()  |   (vm_guid +      |  HvSocketListener::bind  |
|  drives HELLO/AUTH/  |    service_guid)  |  serve() per connection   |
|  PUSH/WriteFile/     |                   |  writes real files,      |
|  LAUNCH/STREAM/Exit  | <---response-----|  launches real process   |
+----------------------+                   +--------------------------+
```

## Scripts

All setup/teardown is scripted. Run them from the repo root.

| Script | Where | Elevated? | Purpose |
|--------|-------|-----------|---------|
| `scripts/setup-host-vmic.ps1` | Host | Yes | Enable "Guest Service Interface" on the VM (opens VMBus channel) |
| `scripts/teardown-host-vmic.ps1` | Host | Yes | Disable "Guest Service Interface" on the VM |
| `scripts/setup-guest-hvsocket.ps1` | Guest | Yes | Start vmicguestinterface + register the service GUID in the registry |
| `scripts/teardown-guest-hvsocket.ps1` | Guest | Yes | Remove the service GUID registry key (+ optionally stop the service) |

## Step 0 - Host: enable Guest Service Interface (one-time)

On the **host** (elevated PowerShell), from the repo root:

```powershell
.\scripts\setup-host-vmic.ps1
# or for a different VM:
.\scripts\setup-host-vmic.ps1 -VmName "your-vm-name"
```

This opens the VMBus channel. Without it, the guest's vmicguestinterface
service cannot start and AF_HYPERV bind() will fail with WSAEACCES.

## Step 1 - Guest: register service GUID + start service (one-time)

On the **guest** (elevated PowerShell), from the repo root:

```powershell
.\scripts\setup-guest-hvsocket.ps1
```

This starts vmicguestinterface and registers the service GUID
`3045196F-2A11-4D65-BCC7-3F9EAB09B7ED` so AF_HYPERV bind() accepts it.

## Step 2 - Guest: build qwan-bootstrapd

In a normal (non-elevated) shell on the guest, from the qwanban workspace:

```powershell
cargo build -p qwanban-stub --bin qwan-bootstrapd --release
```

## Step 3 - Guest: start qwan-bootstrapd (persistent)

```powershell
$workDir = "$env:TEMP\qwan-bootstrapd-work"
New-Item -ItemType Directory -Force -Path $workDir | Out-Null
.\target\release\qwan-bootstrapd.exe `
  --service-guid 3045196F-2A11-4D65-BCC7-3F9EAB09B7ED `
  --work-dir $workDir `
  --secret bootstrap-secret
```

Leave this running. It logs `listening; waiting for host connection...` and
stays up, accepting connections one at a time. **I will keep this process
running so you can iterate.**

## Step 4 - Host: build host-harness

On the host machine, from the qwanban workspace:

```powershell
cargo build -p qwanban-integration --bin host-harness --release
```

## Step 5 - Host: run the harness against the guest

```powershell
.\target\release\host-harness.exe `
  --vm-guid 995A044D-0B4C-424A-9E8A-05EFCE117BE5 `
  --service-guid 3045196F-2A11-4D65-BCC7-3F9EAB09B7ED `
  --secret bootstrap-secret
```

Expected output on success:

```
[harness] connecting to VM 995A044D-... service 3045196F-...
[harness] connected!
[harness] HELLO + AUTH OK
[harness] PUSH_AGENT OK (N bytes)
[harness] WriteFile OK
[harness] LAUNCH OK, collecting STREAM + Exit...
[harness] === RESULT ===
[harness] stdout: "qwan-launched-on-guest\r\n"
[harness] exit_code: Some(0)
```

## Debugging artifacts (on the guest)

After a successful run, inspect what the stub wrote to disk:

```powershell
Get-ChildItem $env:TEMP\qwan-bootstrapd-work -Recurse | Select-Object FullName,Length
Get-Content $env:TEMP\qwan-bootstrapd-work\manifest.json
```

The pushed agent binary appears at `qwan-guest` and the manifest at
`manifest.json` under the work dir.

## Teardown

When you're done with hvsocket integration testing:

```powershell
# Guest (elevated):
.\scripts\teardown-guest-hvsocket.ps1              # remove service GUID
.\scripts\teardown-guest-hvsocket.ps1 -StopService # also stop vmicguestinterface

# Host (elevated):
.\scripts\teardown-host-vmic.ps1                   # disable Guest Service Interface
```

## Troubleshooting

| Symptom | Cause / Fix |
|---------|-------------|
| Guest: `vmicguestinterface` won't start | Host hasn't enabled Guest Service Interface. Run `setup-host-vmic.ps1` on the host (Step 0). |
| Guest: `bind failed: WSAEADDRINUSE` | A previous qwan-bootstrapd is still bound. Kill it and retry. |
| Guest: `bind failed: WSAEACCES` | Service GUID not registered. Run `setup-guest-hvsocket.ps1` (Step 1). |
| Host: `connect failed: WSAECONNREFUSED` | qwan-bootstrapd not running in the guest, or `vmicguestinterface` stopped. |
| Host: `connect failed: WSAETIMEDOUT` | Wrong VM GUID, or the guest's integration services aren't enabled (Step 0). |
| `auth rejected` | `--secret` mismatch between qwan-bootstrapd and harness. |
| harness hangs after LAUNCH | The guest's `echo` command may use a different shell. Check `--work-dir` is writable. |
