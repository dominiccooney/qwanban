# Real hvsocket Integration Test — Runbook

This runbook drives a **real** AF_HYPERV hvsocket bootstrap handshake between
the host and a guest VM running `qwanban-stubd`. No mocks — real sockets, real
file pushes, a real launched process.

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
┌──────────────────────┐                   ┌──────────────────────────┐
│  host-harness binary │ ──AF_HYPERV──────►│  qwanban-stubd (running) │
│  connect_hvsocket()  │   (vm_guid +      │  HvSocketListener::bind  │
│  drives HELLO/AUTH/  │    service_guid)  │  serve() per connection   │
│  PUSH/WriteFile/     │                   │  writes real files,      │
│  LAUNCH/STREAM/Exit  │ ◄──response──────│  launches real process   │
└──────────────────────┘                   └──────────────────────────┘
```

## Step 1 — Guest: one-time prerequisites (elevated, on the guest VM)

These need admin. Run in an **elevated** PowerShell on the guest VM:

```powershell
# 1a. Start the Hyper-V guest service interface (backs AF_HYPERV sockets)
Start-Service vmicguestinterface

# 1b. Register the service GUID so AF_HYPERV bind() accepts it
$key = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Virtualization\GuestCommunicationServices\3045196F-2A11-4D65-BCC7-3F9EAB09B7ED'
New-Item -Path $key -Force | Out-Null
Set-ItemProperty -Path $key -Name 'ElementName' -Value 'qwanban-stubd'

# Verify
Get-Service vmicguestinterface | Select-Object Name,Status
Get-ItemProperty $key | Select-Object ElementName
```

## Step 2 — Guest: build qwanban-stubd

In a normal (non-elevated) shell on the guest, from the qwanban workspace:

```powershell
cd C:\Users\User\clients\cline\qwanban
cargo build -p qwanban-stub --bin stubd --release
```

## Step 3 — Guest: start qwanban-stubd (persistent)

```powershell
$workDir = "$env:TEMP\qwan-stubd-work"
New-Item -ItemType Directory -Force -Path $workDir | Out-Null
.\target\release\stubd.exe `
  --service-guid 3045196F-2A11-4D65-BCC7-3F9EAB09B7ED `
  --work-dir $workDir `
  --secret bootstrap-secret
```

Leave this running. It logs `listening; waiting for host connection...` and
stays up, accepting connections one at a time. **I will keep this process
running so you can iterate.**

## Step 4 — Host: build host-harness

On the host machine, from the qwanban workspace:

```powershell
cargo build -p qwanban-integration --bin host-harness --release
```

## Step 5 — Host: run the harness against the guest

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
Get-ChildItem $env:TEMP\qwan-stubd-work -Recurse | Select-Object FullName,Length
Get-Content $env:TEMP\qwan-stubd-work\manifest.json
```

The pushed agent binary appears at `qwan-guest` and the manifest at
`manifest.json` under the work dir.

## Troubleshooting

| Symptom | Cause / Fix |
|---------|-------------|
| `bind failed: WSAEADDRINUSE` | A previous stubd is still bound. Kill it and retry. |
| `bind failed: WSAEACCES` | Service GUID not registered in the guest registry (Step 1b). |
| `connect failed: WSAECONNREFUSED` | stubd not running in the guest, or `vmicguestinterface` stopped (Step 1a). |
| `connect failed: WSAETIMEDOUT` | Wrong VM GUID, or the guest's integration services aren't enabled. |
| `auth rejected` | `--secret` mismatch between stubd and harness. |
| harness hangs after LAUNCH | The guest's `echo` command may use a different shell. Check `--work-dir` is writable. |
