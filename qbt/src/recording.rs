//! One ordered recording per backend incarnation, independent of live retention.
use std::io::Write;

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::{artifacts::ArtifactStore, journal::JournalEvent};

#[derive(Serialize)]
struct Status<'a> {
    version: u32,
    complete: bool,
    events: u64,
    bytes: u64,
    error: Option<&'a str>,
}

pub(crate) struct Recording {
    directory: cap_std::fs::Dir,
    events: cap_std::fs::File,
    budget: u64,
    bytes: u64,
    count: u64,
    error: Option<String>,
    shutdown: CancellationToken,
}

impl Recording {
    pub(crate) fn new(store: &ArtifactStore, budget: u64, shutdown: CancellationToken) -> Result<Self> {
        ensure!(budget > 0, "journal byte limit must be greater than zero");
        let directory = store.journal_directory()?;
        directory.create_dir("screenshots")?;
        let events = directory.create("events.jsonl")?;
        let recording = Self { directory, events, budget, bytes: 0, count: 0, error: None, shutdown };
        recording.status(false)?;
        Ok(recording)
    }

    fn status(&self, complete: bool) -> Result<()> {
        let status = serde_json::to_vec(&Status {
            version: 1, complete, events: self.count, bytes: self.bytes, error: self.error.as_deref(),
        })?;
        let mut file = self.directory.create("status.tmp")?;
        file.write_all(&status)?;
        file.sync_all()?;
        drop(file);
        self.directory.rename("status.tmp", &self.directory, "status.json")?;
        Ok(())
    }

    fn write(&mut self, event: &JournalEvent, screenshot: Option<&[u8]>) -> Result<()> {
        let mut json = serde_json::to_vec(event)?;
        json.push(b'\n');
        let size = json.len() as u64 + screenshot.map_or(0, |png| png.len() as u64);
        ensure!(size <= self.budget.saturating_sub(self.bytes), "journal byte limit reached");
        if let Some(png) = screenshot {
            let id = event.screenshot_id.as_ref().context("recorded screenshot has no ID")?;
            let mut file = self.directory.create(format!("screenshots/{id}.png"))?;
            file.write_all(png)?;
            file.sync_data()?;
        }
        self.events.write_all(&json)?;
        self.events.sync_data()?;
        self.bytes += size;
        self.count += 1;
        Ok(())
    }

    /// Called inside the journal lock before broadcasting the same event.
    /// Native actions may already have executed; failure stops recording/work,
    /// not a promise that an action can safely be retried.
    pub(crate) fn append(&mut self, event: &JournalEvent, screenshot: Option<&[u8]>) {
        if self.error.is_some() { return; }
        if let Err(error) = self.write(event, screenshot) {
            self.error = Some(format!("{error:#}"));
            eprintln!("qbt journal recording failed: {error:#}; stopping backend");
            let _ = self.status(false);
            self.shutdown.cancel();
        }
    }

    pub(crate) fn finish(&mut self) -> Result<()> {
        if let Some(error) = &self.error { anyhow::bail!("qbt journal recording incomplete: {error}"); }
        self.events.sync_all()?;
        self.status(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Journal;
    use std::sync::Arc;

    #[test]
    fn recording_survives_live_eviction_and_backend_restarts_without_overwrite() {
        let root = std::env::temp_dir().join(format!("qbt-recording-{}-{}", std::process::id(), crate::journal::now_ms()));
        let store = ArtifactStore::new(&root).unwrap();
        for _ in 0..2 {
            let shutdown = CancellationToken::new();
            let recording = Recording::new(&store, 1024 * 1024, shutdown.clone()).unwrap();
            let journal = Journal::recording(1, recording);
            let mut threads = Vec::new();
            for writer in 0..4 {
                let journal = Arc::clone(&journal);
                threads.push(std::thread::spawn(move || {
                    for index in 0..150 { journal.append("test", serde_json::json!({"writer":writer,"index":index}), Some(vec![1, 2, 3])); }
                }));
            }
            for thread in threads { thread.join().unwrap(); }
            assert_eq!(journal.subscribe_with_snapshot().1.len(), 500);
            assert!(journal.screenshot("shot_1").is_none());
            journal.finish_recording().unwrap();
            assert!(!shutdown.is_cancelled());
        }
        let incarnations: Vec<_> = std::fs::read_dir(root.join("journal")).unwrap().map(|entry| entry.unwrap().path()).collect();
        assert_eq!(incarnations.len(), 2);
        for path in incarnations {
            let lines = std::fs::read_to_string(path.join("events.jsonl")).unwrap();
            let events: Vec<serde_json::Value> = lines.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
            assert_eq!(events.len(), 600);
            for (index, event) in events.iter().enumerate() {
                assert_eq!(event["seq"], index + 1);
                let id = event["screenshotId"].as_str().unwrap();
                assert_eq!(std::fs::read(path.join(format!("screenshots/{id}.png"))).unwrap(), vec![1, 2, 3]);
            }
            let status: serde_json::Value = serde_json::from_slice(&std::fs::read(path.join("status.json")).unwrap()).unwrap();
            assert_eq!(status["complete"], true);
            assert_eq!(status["events"], 600);
        }
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recording_limit_stops_backend_and_cannot_be_finalized_as_complete() {
        let root = std::env::temp_dir().join(format!("qbt-record-limit-{}-{}", std::process::id(), crate::journal::now_ms()));
        let store = ArtifactStore::new(&root).unwrap();
        let shutdown = CancellationToken::new();
        let journal = Journal::recording(1, Recording::new(&store, 1, shutdown.clone()).unwrap());
        journal.append("test", serde_json::json!({}), Some(vec![1]));
        assert!(shutdown.is_cancelled());
        assert!(journal.finish_recording().is_err());
        drop(journal);
        let path = std::fs::read_dir(root.join("journal")).unwrap().next().unwrap().unwrap().path();
        let status: serde_json::Value = serde_json::from_slice(&std::fs::read(path.join("status.json")).unwrap()).unwrap();
        assert_eq!(status["complete"], false);
        assert!(status["error"].as_str().unwrap().contains("byte limit"));
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn serve_records_events_and_marks_status_complete_on_shutdown_backend() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::{TcpListener, TcpStream};

        let root = std::env::temp_dir().join(format!(
            "qbt-serve-record-{}-{}",
            std::process::id(),
            crate::journal::now_ms()
        ));
        let store = Arc::new(ArtifactStore::new(&root).unwrap());
        let shutdown = CancellationToken::new();
        let recording = Recording::new(&store, 1024 * 1024, shutdown.clone()).unwrap();
        let journal = Journal::recording(10, recording);
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(crate::computer_use::serve_agent(
            listener,
            journal.clone(),
            store.clone(),
            shutdown.clone(),
        ));

        let stream = TcpStream::connect(addr).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        writer
            .write_all(b"{\"id\":1,\"action\":\"publish_event\",\"kind\":\"transcript.message\",\"payload\":{\"text\":\"hello\"}}\n")
            .await
            .unwrap();
        let reply: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(reply["ok"], true);

        let shutdown_stream = TcpStream::connect(addr).await.unwrap();
        let (shutdown_reader, mut shutdown_writer) = shutdown_stream.into_split();
        let mut shutdown_lines = BufReader::new(shutdown_reader).lines();
        shutdown_writer
            .write_all(b"{\"id\":2,\"action\":\"shutdown_backend\"}\n")
            .await
            .unwrap();
        let shutdown_reply: serde_json::Value =
            serde_json::from_str(&shutdown_lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(shutdown_reply["ok"], true);

        shutdown.cancelled().await;
        server.await.unwrap();
        journal.finish_recording().unwrap();

        let path = std::fs::read_dir(root.join("journal"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let status: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.join("status.json")).unwrap()).unwrap();
        assert_eq!(status["complete"], true);
        assert_eq!(status["events"], 1);
        assert!(status["error"].is_null());

        let events = std::fs::read_to_string(path.join("events.jsonl")).unwrap();
        let event: serde_json::Value = serde_json::from_str(events.lines().next().unwrap()).unwrap();
        assert_eq!(event["kind"], "transcript.message");
        assert_eq!(event["payload"]["text"], "hello");

        drop(journal);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}