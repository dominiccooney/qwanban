//! Builds the per-case `Manifest` from a `JobSpec` + resolved image + endpoints.
//! Pure logic, fully unit-testable.

use crate::job::JobSpec;
use qwanban_proto::id::{CaseId, JobId};
use qwanban_proto::manifest::*;

/// Inputs the orchestrator resolves before building the manifest.
pub struct ManifestInputs<'a> {
    pub job_id: JobId,
    pub case_id: CaseId,
    pub spec: &'a JobSpec,
    pub broker_endpoint: String,
    pub broker_cert_spki: String,
    pub inference_base_url: String,
    pub proxy_url: String,
    pub proxy_ca_fpr: String,
    pub allowed_models: Vec<String>,
    pub agent: AgentSpec,
    pub limits: LimitsSpec,
}

pub fn build(input: ManifestInputs<'_>) -> Manifest {
    let (script_text, report_text) = match input.spec.kind {
        crate::job::JobKind::ScriptedQa => (Some(input.spec.task_text.clone()), None),
        crate::job::JobKind::BugFix => (None, Some(input.spec.task_text.clone())),
    };
    Manifest {
        schema: "qwan.manifest/v1".to_string(),
        job_id: input.job_id,
        case_id: input.case_id,
        kind: match input.spec.kind {
            crate::job::JobKind::ScriptedQa => JobKind::ScriptedQa,
            crate::job::JobKind::BugFix => JobKind::BugFix,
        },
        task: TaskPayload {
            script_text,
            report_text,
            note: input.spec.note.clone(),
        },
        repo: RepoSpec {
            url: String::new(), // resolved by agent at runtime from git_ref
            ref_: input.spec.git_ref.clone(),
            checkout_path: "/work/app".to_string(),
        },
        broker: BrokerEndpoint {
            endpoint: input.broker_endpoint.clone(),
            cert_spki_sha256: input.broker_cert_spki.clone(),
        },
        auth: AuthSpec {
            case_token_file: "/qwan/case.token".to_string(),
        },
        inference: InferenceSpec {
            base_url: input.inference_base_url.clone(),
            dummy_key: "DUMMY".to_string(),
            allowed_models: input.allowed_models.clone(),
        },
        proxy: ProxySpec {
            https_proxy: input.proxy_url.clone(),
            ca_fpr_sha256: input.proxy_ca_fpr.clone(),
        },
        agent: input.agent,
        capture: CaptureSpec {
            fps: 5,
            segment_seconds: 4,
            encode_where: EncodeWhere::Guest,
        },
        limits: input.limits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobKind;

    fn dummy_spec(kind: JobKind) -> JobSpec {
        JobSpec {
            kind,
            base_image: "linux-test".into(),
            git_ref: "main".into(),
            task_text: "do the thing".into(),
            note: None,
            caps: None,
        }
    }

    #[test]
    fn scripted_qa_puts_text_in_script_text() {
        let spec = dummy_spec(JobKind::ScriptedQa);
        let m = build(ManifestInputs {
            job_id: JobId::from_str_inner("job_1"),
            case_id: CaseId::from_str_inner("case_1"),
            spec: &spec,
            broker_endpoint: "https://10.0.75.1:7443".into(),
            broker_cert_spki: "abc".into(),
            inference_base_url: "https://10.0.75.1:7444/v1".into(),
            proxy_url: "http://10.0.75.1:8080".into(),
            proxy_ca_fpr: "def".into(),
            allowed_models: vec!["m1".into()],
            agent: AgentSpec {
                files: vec![],
                launch: AgentLaunch {
                    shell: "bash".into(),
                    command: "true".into(),
                    cwd: "/qwan".into(),
                    env: Default::default(),
                },
            },
            limits: LimitsSpec { max_runtime_s: 60 },
        });
        assert!(m.task.script_text.is_some());
        assert!(m.task.report_text.is_none());
        assert_eq!(m.schema, "qwan.manifest/v1");
        assert_eq!(m.inference.dummy_key, "DUMMY");
    }
}
