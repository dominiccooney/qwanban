//! `WebReport` — the read-only per-job page (§artifact-store-and-clipping 7.9).
//! The trait abstracts rendering so the real HTML/JS player is isolated; the
//! stub impl wires the data (transcript entries + clips) into a minimal page.

use crate::transcript_index::TranscriptIndex;
use async_trait::async_trait;
use qwanban_proto::id::CaseId;
use qwanban_proto::transcript::TranscriptEntry;
use qwanban_proto::QwanResult;
use std::path::PathBuf;

/// Renders a read-only job/case report page.
#[async_trait]
pub trait WebReport: Send + Sync {
    async fn render_case(&self, case_id: &CaseId) -> QwanResult<String>;
}

/// A minimal stub renderer: pulls the transcript + clip list, emits a basic
/// HTML page. Full scrubber/JS is out of scope for v1.
pub struct StubWebReport {
    root: PathBuf,
    transcript: TranscriptIndex,
}

impl StubWebReport {
    pub fn new(root: impl Into<PathBuf>, transcript: TranscriptIndex) -> Self {
        Self { root: root.into(), transcript }
    }

    fn clip_dir(&self, case_id: &CaseId) -> PathBuf {
        self.root.join("cases").join(case_id.as_str()).join("clips")
    }
}

#[async_trait]
impl WebReport for StubWebReport {
    async fn render_case(&self, case_id: &CaseId) -> QwanResult<String> {
        let entries = self.transcript.all(case_id)?;
        let clip_names: Vec<String> = match std::fs::read_dir(self.clip_dir(case_id)) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.ends_with(".json"))
                .collect(),
            Err(_) => Vec::new(),
        };
        let mut html = String::new();
        html.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\">");
        html.push_str(&format!("<title>qwan report — case {}</title></head><body>", case_id.as_str()));
        html.push_str(&format!("<h1>Case {}</h1>", case_id.as_str()));
        html.push_str("<h2>Transcript</h2><ol>");
        for e in &entries {
            html.push_str(&format!("<li>{}</li>", html_escape(&entry_summary(e))));
        }
        html.push_str("</ol>");
        html.push_str("<h2>Clips</h2><ul>");
        for name in &clip_names {
            html.push_str(&format!("<li><a href=\"clips/{name}\">{name}</a></li>"));
        }
        html.push_str("</ul></body></html>");
        Ok(html)
    }
}

fn entry_summary(e: &TranscriptEntry) -> String {
    match e {
        TranscriptEntry::Breadcrumb(b) => format!("[{}] {} (t={}ns)", kind_str(b.kind), b.label, b.timeline_ns),
        TranscriptEntry::ToolIo { summary, timeline_ns, .. } => format!("tool: {summary} (t={timeline_ns}ns)"),
        TranscriptEntry::Log { level, message, timeline_ns, .. } => format!("{level:?}: {message} (t={timeline_ns}ns)"),
    }
}

fn kind_str(k: qwanban_proto::transcript::BreadcrumbKind) -> &'static str {
    use qwanban_proto::transcript::BreadcrumbKind::*;
    match k {
        StepBegin => "step_begin",
        StepEnd => "step_end",
        Assertion => "assertion",
        Action => "action",
        Note => "note",
        Bug => "bug",
        Fix => "fix",
        Error => "error",
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use qwanban_proto::id::BreadcrumbId;
    use qwanban_proto::transcript::{Breadcrumb, BreadcrumbKind};

    #[tokio::test]
    async fn render_contains_breadcrumb_labels() {
        let dir = tempfile::tempdir().unwrap();
        let ti = TranscriptIndex::new(dir.path());
        let c = CaseId::from_str_inner("c1");
        ti.append(&TranscriptEntry::Breadcrumb(Breadcrumb {
            breadcrumb_id: BreadcrumbId("b1".into()),
            case_id: c.clone(),
            kind: BreadcrumbKind::Bug,
            label: "repro the crash".into(),
            timeline_ns: 123,
            detail: None,
        }))
        .await
        .unwrap();
        let wr = StubWebReport::new(dir.path(), ti);
        let html = wr.render_case(&c).await.unwrap();
        assert!(html.contains("repro the crash"));
        assert!(html.contains("case c1"));
    }

    #[tokio::test]
    async fn render_escapes_html() {
        let dir = tempfile::tempdir().unwrap();
        let ti = TranscriptIndex::new(dir.path());
        let c = CaseId::from_str_inner("c1");
        ti.append(&TranscriptEntry::Breadcrumb(Breadcrumb {
            breadcrumb_id: BreadcrumbId("b1".into()),
            case_id: c.clone(),
            kind: BreadcrumbKind::Note,
            label: "<script>x</script>".into(),
            timeline_ns: 0,
            detail: None,
        }))
        .await
        .unwrap();
        let wr = StubWebReport::new(dir.path(), ti);
        let html = wr.render_case(&c).await.unwrap();
        assert!(!html.contains("<script>x</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
