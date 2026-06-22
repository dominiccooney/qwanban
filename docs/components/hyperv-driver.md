# Component: Hyper-V Driver (`qwanban-hyperv`)

> Owns the host↔hypervisor surface: VM lifecycle, disks, networking, TCP
> bootstrap channel, and checkpoints. Read [`README.md`](README.md) §S1–S8.
> Implements design.md §5.1–5.3, §5.5–5.6.

## Purpose & scope

A safe, async Rust wrapper over Hyper-V that the orchestrator uses to create,
boot, hold, migrate, and destroy ephemeral case VMs cloned from
maintainer-supplied VHD/VHDX files. Also owns the **TCP** transport used for
bootstrap (push + launch in 7.1) over the private vSwitch.

This component does **not** know about jobs, manifests, or agents — it operates
on VMs and disks. `qwanban-core`/agent-lifecycle layer those on top.

## Sequence coverage

Owns: **7.1.6–7.1.12** (create/define/start/await-bootstrap), the TCP
transport under **7.1.13–7.1.16**, **7.1.E1–E2**, **7.10.5** (checkpoint),
**7.11.6, 7.11.10** (sibling VM create / old VM destroy), **7.12.8–7.12.9**
(stop/destroy/checkpoint).

## Dependencies

- Upstream: none (lowest layer). Consumes `qwanban.toml` image registry +
  `ResourceCaps`.
- Downstream: `qwanban-core` (orch), agent-lifecycle (uses the TCP channel).

## Backend strategy

Two interchangeable backends behind one `HyperVDriver` trait:

1. **WMI v2** (`root\virtualization\v2`) via the `windows`/`wmi` crates —
   preferred for fine-grained, programmatic control and event subscriptions
   (state changes).
2. **PowerShell Hyper-V module** shelling out (`New-VM`, `New-VHD -Differencing`,
   `Set-VMProcessor`, `Set-VMMemory`, `Checkpoint-VM`, `Stop-VM`, `Remove-VM`) —
   used as bootstrap / fallback and for operations awkward in raw WMI.

Selection is config (`hyperv.backend = "wmi" | "powershell"`); default `wmi`,
fall back to `powershell` per-operation if a WMI call is unimplemented.

## Disk model (fast ephemerality)

- Base image = read-only **parent** VHD/VHDX at the maintainer's path (S7 /
  §5.1). Driver never mutates it.
- Each case gets a **differencing AVHDX** child created at
  `caps.disk_root/<case_id>.avhdx` with the registered parent.
- Optional **golden checkpoint**: if the image registry entry has
  `booted_checkpoint = true`, the driver clones from a "booted + bootstrap ready"
  checkpoint of a warm VM instead of cold-booting (skips 7.1.11 boot wait).
- Teardown deletes the AVHDX (parent untouched).

## Networking

- Ensures a dedicated **internal vSwitch** `qwan-internal` exists (created once;
  idempotent). Host holds a static IP on it (the broker/proxy bind here).
- Each case NIC attaches to `qwan-internal`. The driver applies a **host firewall
  policy** so the guest can reach ONLY the broker + proxy ports; all other egress
  is dropped (design.md §4.1, §5.2). Allowed CIDR/ports come from config.
- DHCP/static: driver assigns the guest a static lease (or injects IP via the
  bootstrap files) so the broker endpoint is reachable immediately.

## TCP bootstrap channel

- Owns a `TcpStream` dialer that connects to the guest's IP on the private
  vSwitch — no Hyper-V sockets (AF_HYPERV) required, avoiding host admin
  elevation.
- Exposes a byte-stream + simple length-prefixed framing used by agent-lifecycle
  to implement `push_agent`, `write_files`, `launch_agent` (7.1.13–7.1.16). The
  *protocol* over this channel is owned by agent-lifecycle / stub-loader; the
  *transport* is owned here.
- The in-guest peer is the **`qwan-stub` loader** baked into every base image
  (see [`stub-loader.md`](stub-loader.md)). **No SSH** — the driver dials the
  guest's vSwitch IP + well-known port.

## Driver trait (interface)

```rust
#[async_trait]
pub trait HyperVDriver: Send + Sync {
    async fn ensure_switch(&self, name: &str) -> Result<()>;
    async fn create_case_vm(&self, spec: VmSpec) -> Result<VmHandle>;     // 7.1.6-7.1.8
    async fn start_vm(&self, vm: &VmHandle) -> Result<()>;                // 7.1.10
    async fn await_state(&self, vm: &VmHandle, s: VmState, t: Duration) -> Result<()>; // 7.1.11
    async fn open_stream(&self, vm: &VmHandle, port: u32) -> Result<GuestStream>; // 7.1.12
    async fn checkpoint(&self, vm: &VmHandle, name: &str) -> Result<CheckpointId>; // 7.10.5
    async fn stop_vm(&self, vm: &VmHandle, mode: StopMode) -> Result<()>; // 7.12.8
    async fn destroy_case_vm(&self, vm: &VmHandle) -> Result<()>;         // delete VM + AVHDX
    fn subscribe_state(&self, vm: &VmHandle) -> BoxStream<'_, VmState>;   // WMI events
}

pub struct VmSpec {
    pub case_id: String,
    pub base_vhd: PathBuf,         // from image registry
    pub caps: ResourceCaps,        // vcpu, memory_mb, dyn-mem max, disk_gb
    pub switch: String,            // "qwan-internal"
    pub firewall: FirewallPolicy,  // allowed host ports
}
pub struct VmHandle { pub vm_id: String, pub guest_ip: IpAddr, pub avhdx: PathBuf }
pub enum VmState { Defined, Booting, Running, Saved, Off, Error(String) }
```

`ResourceCaps` maps to `Set-VMProcessor -Count`, `Set-VMMemory` (startup +
maximum for dynamic memory), and AVHDX size. A **host watchdog** (owned by
`qwanban-core`, not here) enforces `max_runtime`; this driver only provides
`stop_vm`.

## Concurrency / scheduling

- Driver operations are independently async. The **hard `max_concurrent_cases`
  cap is enforced by `qwanban-core` at `submit`** (synchronous accept/reject,
  **no queue** — §5.8), not here. The driver exposes current VM count so core can
  count free slots.

## Testing

- **Unit (mocked WMI/PowerShell):** command construction for create/caps/destroy.
- **Integration (requires Hyper-V host, gated `#[ignore]`):** full
  create→start→TCP-echo→destroy on a tiny Linux test image; assert AVHDX
  created then deleted, parent untouched.
- **Firewall test:** from inside a test guest, assert broker/proxy reachable and
  an arbitrary external host blocked.
- **Checkpoint/restore:** intervention hold path keeps the VM after case end.

## Open items

- Whether to pre-warm a pool of booted VMs per image for latency (post-v1).
