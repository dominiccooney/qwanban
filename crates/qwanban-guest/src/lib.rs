//! `qwanban-guest` — the in-VM qwan agent (§agent-lifecycle, §breadcrumbs-transcript,
//! §video-capture-encode, §input-injection, §mcp-server). Hosts:
//! - the **supervisor** over subsystems,
//! - the **broker client** (registers, heartbeats, ingests),
//! - **breadcrumbs/transcript** sink,
//! - the **computer-use executor** (`cuxec`) behind a trait,
//! - the **qwan MCP server** (qwan-only tools: breadcrumb/clip/handoff/finish).
//!
//! Host-touching capture/input backends are trait seams; the timeline-join
//! bookkeeping is pure and unit-testable here.

pub mod broker_client;
pub mod breadcrumbs;
pub mod mcp;
pub mod computer_use;
pub mod supervisor;

pub use breadcrumbs::{BreadcrumbSink, BreadcrumbTable};
pub use computer_use::{ComputerUseExecutor, ComputerBackend};
pub use mcp::QwanMcpServer;
