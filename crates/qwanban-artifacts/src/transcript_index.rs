//! `TranscriptIndex` — append-only per-case transcript log (JSONL), queryable
//! by timeline range. Each line is a serialized `TranscriptEntry`. The index is
//! loaded into memory on open for range queries (transcripts are small).

use qwanban_proto::id::CaseId;
use qwanban_proto::timeline::TimelineNs;
use qwanban_proto::transcript::TranscriptEntry;
use qwanban_proto::QwanResult;
use std::path::{Path, PathBuf};

pub struct TranscriptIndex {
    root: PathBuf,
}

impl TranscriptIndex {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn case_dir(&self, case_id: &CaseId) -> PathBuf {
        self.root.join("cases").join(case_id.as_str())
    }

    fn log_path(&self, case_id: &CaseId) -> PathBuf {
        self.case_dir(case_id).join("transcript.log")
    }

    fn timeline_ns_of(e: &TranscriptEntry) -> TimelineNs {
        match e {
            TranscriptEntry::Breadcrumb(b) => b.timeline_ns,
            TranscriptEntry::ToolIo { timeline_ns, .. } => *timeline_ns,
            TranscriptEntry::Log { timeline_ns, .. } => *timeline_ns,
        }
    }

    /// Append one entry (serialized as a JSONL line).
    pub async fn append(&self, entry: &TranscriptEntry) -> QwanResult<()> {
        let path = self.log_path(&match entry {
            TranscriptEntry::Breadcrumb(b) => b.case_id.clone(),
            TranscriptEntry::ToolIo { case_id, .. } => case_id.clone(),
            TranscriptEntry::Log { case_id, .. } => case_id.clone(),
        });
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| qwanban_proto::internal(format!("mkdir: {e}")))?;
        }
        let line = serde_json::to_string(entry).map_err(|e| qwanban_proto::internal(format!("serde: {e}")))?;
        let mut content = line.into_bytes();
        content.push(b'\n');
        use tokio::io::AsyncWriteExt;
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|e| qwanban_proto::internal(format!("open {}: {e}", path.display())))?;
        f.write_all(&content).await.map_err(|e| qwanban_proto::internal(format!("write: {e}")))?;
        Ok(())
    }

    /// Read all entries for a case, in order.
    pub fn all(&self, case_id: &CaseId) -> QwanResult<Vec<TranscriptEntry>> {
        read_jsonl(&self.log_path(case_id))
    }

    /// Entries with `timeline_ns` in `[from, to)`.
    pub fn range(&self, case_id: &CaseId, from: TimelineNs, to: TimelineNs) -> QwanResult<Vec<TranscriptEntry>> {
        let entries = self.all(case_id)?;
        Ok(entries
            .into_iter()
            .filter(|e| {
                let ts = Self::timeline_ns_of(e);
                ts >= from && ts < to
            })
            .collect())
    }
}

fn read_jsonl(path: &Path) -> QwanResult<Vec<TranscriptEntry>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(qwanban_proto::internal(format!("read {}: {e}", path.display()))),
    };
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| qwanban_proto::internal(format!("parse: {e}"))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qwanban_proto::id::BreadcrumbId;
    use qwanban_proto::transcript::{Breadcrumb, BreadcrumbKind};

    fn label_of(e: &TranscriptEntry) -> &str {
        match e {
            TranscriptEntry::Breadcrumb(b) => &b.label,
            TranscriptEntry::ToolIo { summary, .. } => summary,
            TranscriptEntry::Log { message, .. } => message,
        }
    }

    fn bc(case: &CaseId, id: &str, ts: i64, label: &str) -> TranscriptEntry {
        TranscriptEntry::Breadcrumb(Breadcrumb {
            breadcrumb_id: BreadcrumbId(id.into()),
            case_id: case.clone(),
            kind: BreadcrumbKind::StepBegin,
            label: label.into(),
            timeline_ns: ts,
            detail: None,
        })
    }

    #[tokio::test]
    async fn append_then_all_preserves_order() {
        let dir = tempfile::tempdir().unwrap();
        let idx = TranscriptIndex::new(dir.path());
        let c = CaseId::from_str_inner("c1");
        idx.append(&bc(&c, "b1", 100, "first")).await.unwrap();
        idx.append(&bc(&c, "b2", 200, "second")).await.unwrap();
        let all = idx.all(&c).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(label_of(&all[0]), "first");
    }

    #[tokio::test]
    async fn range_returns_slice() {
        let dir = tempfile::tempdir().unwrap();
        let idx = TranscriptIndex::new(dir.path());
        let c = CaseId::from_str_inner("c1");
        idx.append(&bc(&c, "b1", 100, "a")).await.unwrap();
        idx.append(&bc(&c, "b2", 200, "b")).await.unwrap();
        idx.append(&bc(&c, "b3", 300, "c")).await.unwrap();
        let mid = idx.range(&c, 150, 300).unwrap();
        assert_eq!(mid.len(), 1);
        assert_eq!(label_of(&mid[0]), "b");
    }

    #[tokio::test]
    async fn all_missing_case_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let idx = TranscriptIndex::new(dir.path());
        let c = CaseId::from_str_inner("nope");
        assert!(idx.all(&c).unwrap().is_empty());
    }
}
