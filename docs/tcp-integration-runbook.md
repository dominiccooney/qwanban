# Real TCP Integration Test - Runbook

This runbook drives a **real** TCP bootstrap handshake between the host and a
guest VM running `qwan-bootstrapd` (the production stub loader, packaged as a
daemon). No mocks - real sockets, real file pushes, a real launched process.

The guest VM IP on the private vSwitch is:

```
172.18.72.105
```

The TCP port the stub listens on:

```
7474
```

## Architecture

```
HOST (your machine)                    GUEST (this dev VM, persistent)
+----------------------+               +--------------------------+
|  host-harness binary | --TCP:7474-->|  qwan-bootstrapd (run)   |
|  TcpStream::connect  |   (private    |  TcpListener::bind       |
|  drives HELLO/AUTH/  |    vSwitch)   |  serve() per connection   |
|  PUSH/WriteFile/     |               |  writes real files,      |
|  LAUNCH/STREAM/Exit  | <--response--|  launches real process   |
+----------------------+               +--------------------------+
```

No elevation required on either side for the transport. Only VM lifecycle
(create/start/stop/destroy) needs elevation on the host.

## Step 1 - Guest: build qwan-bootstrapd

In a shell on the guest, from the qwanban workspace:

```powershell
cargo build -p qwanban-stub --bin qwan-bootstrapd
```

## Step 2 - Guest: start qwan-bootstrapd (persistent)

```powershell
.\scripts\start-bootstrapd.ps1
```

Leave this running. It binds `0.0.0.0:7474` and stays up, accepting connections
one at a time.

## Step 3 - Host: build host-harness

On the host machine, from the qwanban workspace:

```powershell
cargo build -p qwanban-integration --bin host-harness
```

## Step 4 - Host: run the harness against the guest

```powershell
.\target\debug\host-harness.exe `
  --addr 172.18.72.105:7474 `
  --secret bootstrap-secret
```

Expected output on success:

```
[harness] connecting to 172.18.72.105:7474 ...
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

```powershell
# Guest:
.\scripts\stop-bootstrapd.ps1
```

## Troubleshooting

| Symptom | Cause / Fix |
|---------|-------------|
| `connect failed: connection refused` | qwan-bootstrapd not running in the guest. Run `start-bootstrapd.ps1`. |
| `connect failed: timed out` | Wrong IP, or firewall blocking port 7474. Check `Get-NetFirewallRule`. |
| `auth rejected` | `--secret` mismatch between qwan-bootstrapd and harness. |
| harness hangs after LAUNCH | The guest's `echo` command may use a different shell. Check `--work-dir` is writable. |
