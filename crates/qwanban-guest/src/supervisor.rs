//! The qwan agent supervisor (§agent-lifecycle guest side). Boots subsystems
//! (broker client, breadcrumbs, capture, cuxec, qwan MCP), materializes the agent
//! files + runs the launch command, and supervises the launched process.
//!
//! v1: the supervisor owns the BreadcrumbTable + the MCP tool dispatcher. The
//! OS-specific capture/input backends + the real broker HTTP client are trait
//! seams; tests inject mocks. The launched agent (Cline) connects to the qwan
//! MCP server over loopback.

use crate::breadcrumbs::BreadcrumbTable;
use crate::mcp::QwanMcpServer;
use qwanban_proto::id::CaseId;
use qwanban_proto::manifest::Manifest;
use qwanban_proto::QwanResult;
use std::sync::Arc;

/// The assembled guest subsystems, returned by `boot`.
pub struct GuestRuntime {
    pub case_id: CaseId,
    pub breadcrumbs: Arc<BreadcrumbTable>,
    pub mcp: QwanMcpServer,
}

/// Boot the guest subsystems from a parsed manifest.
pub fn boot(manifest: &Manifest) -> QwanResult<GuestRuntime> {
    let breadcrumbs = Arc::new(BreadcrumbTable::new(manifest.case_id.clone()));
    let mcp = QwanMcpServer::new(breadcrumbs.clone());
    Ok(GuestRuntime { case_id: manifest.case_id.clone(), breadcrumbs, mcp })
}

/// Run the guest supervisor: boot subsystems, then run the agent launch command
/// and supervise it. v1: boots + returns.
pub async fn run(manifest: Manifest) -> QwanResult<()> {
    let _rt = boot(&manifest)?;
    // TODO(M2): spawn agent.launch.command, supervise, register+heartbeat, forward exit.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qwanban_proto::id::JobId;
    use qwanban_proto::manifest::*;

    fn manifest() -> Manifest {
        Manifest {
            schema: "qwan.manifest/v1".into(),
            job_id: JobId::from_str_inner("job_1"),
            case_id: CaseId::from_str_inner("case_1"),
            kind: JobKind::ScriptedQa,
            task: TaskPayload { script_text: Some("do thing".into()), report_text: None, note: None },
            repo: RepoSpec { url: "".into(), ref_: "main".into(), checkout_path: "/work/app".into() },
            broker: BrokerEndpoint { endpoint: "https://10.0.75.1:7443".into(), cert_spki_sha256: "abc".into() },
            auth: AuthSpec { case_token_file: "/qwan/case.token".into() },
            inference: InferenceSpec { base_url: "http://10.0.75.1:1234/v1".into(), dummy_key: "DUMMY".into(), allowed_models: vec!["m1".into()] },
            proxy: ProxySpec { https_proxy: "http://10.0.75.1:8080".into(), ca_fpr_sha256: "def".into() },
            agent: AgentSpec { files: vec![], launch: AgentLaunch { shell: "bash".into(), command: "true".into(), cwd: "/qwan".into(), env: Default::default() } },
            capture: CaptureSpec { fps: 5, segment_seconds: 4, encode_where: EncodeWhere::Guest },
            limits: LimitsSpec { max_runtime_s: 3600 },
        }
    }

    #[tokio::test]
    async fn boot_creates_runtime_with_breadcrumbs_and_mcp() {
        let m = manifest();
        let rt = boot(&m).unwrap();
        assert_eq!(rt.case_id, CaseId::from_str_inner("case_1"));
    }
}
