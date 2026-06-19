//! The qwan agent supervisor (§agent-lifecycle guest side). Boots subsystems
//! (broker client, breadcrumbs, capture, cuxec, qwan MCP), materializes the agent
//! files + runs the launch command, and supervises the launched process.

use qwanban_proto::manifest::Manifest;

/// Run the guest supervisor against a parsed manifest. (Stub: teammates fill in.)
pub async fn run(_manifest: Manifest) -> qwanban_proto::QwanResult<()> {
    // TODO(M2): boot subsystems, run agent.launch.command, supervise, register+heartbeat.
    Ok(())
}
