//! Bounded disk-backed journal.
//!
//! The journal is the single source of truth the observatory reads. Two
//! producers write to it: the agent server journals computer actions and
//! screenshots as it executes them (it is the only component that sees them
//! all, in execution order), and the agent publishes its own events
//! (transcripts, coordinator status changes) over the same socket. Observers are
//! pure subscribers: they take a snapshot plus a live stream, both in one
//! total order (`seq`).
//!
//! Events and full-screen PNGs are persisted under `<artifact-root>/journal/`
//! (`events.jsonl` and `screenshots/shot_<seq>.png`) and bounded as a sliding
//! window: when screenshots or events age past the retention window, the
//! oldest files are unlinked and `events.jsonl` is compacted. Reopening the
//! journal in the same artifact root resumes `seq` monotonically so backend
//! restarts neither lose recent context nor accumulate unbounded files.

use crate::artifacts::ArtifactStore;
use anyhow::Context;
use cap_std::fs::{Dir, File, OpenOptions};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

pub(crate) const MAX_EVENTS: usize = 2_000;
const BROADCAST_CAPACITY: usize = 512;

pub(crate) const DEFAULT_MAX_SCREENSHOTS: usize = 500;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JournalEvent {
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub at_ms: u64,
    pub kind: String,
    pub payload: serde_json::Value,
    /// Set when the event captured a screenshot; fetch it by id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_id: Option<String>,
}

struct State {
    directory: Dir,
    events_file: Option<File>,
    lines_on_disk: usize,
    events: VecDeque<Arc<JournalEvent>>,
    screenshots: VecDeque<String>,
    next_seq: u64,
}

pub(crate) struct Journal {
    state: Mutex<Option<State>>,
    sender: broadcast::Sender<Arc<JournalEvent>>,
    max_events: usize,
    max_screenshots: usize,
    #[cfg(test)]
    temp_root: Option<std::path::PathBuf>,
}

impl Journal {
    pub(crate) fn open(
        artifacts: &ArtifactStore,
        max_screenshots: usize,
    ) -> anyhow::Result<Arc<Self>> {
        Self::open_with_limits(
            artifacts,
            MAX_EVENTS.max(max_screenshots),
            max_screenshots,
            #[cfg(test)]
            None,
        )
    }

    fn open_with_limits(
        artifacts: &ArtifactStore,
        max_events: usize,
        max_screenshots: usize,
        #[cfg(test)] temp_root: Option<std::path::PathBuf>,
    ) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(max_events > 0, "max_events must be greater than zero");
        anyhow::ensure!(
            max_screenshots > 0,
            "--max-screenshots must be greater than zero"
        );
        let directory = artifacts.journal_directory()?;
        directory.create_dir_all("screenshots")?;
        let _ = directory.remove_file("events.tmp");

        let mut events = VecDeque::new();
        let mut next_seq = 1u64;
        if let Ok(file) = directory.open("events.jsonl") {
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let Ok(line) = line else {
                    break;
                };
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<JournalEvent>(&line) {
                    Ok(event) => {
                        next_seq = next_seq.max(event.seq.saturating_add(1));
                        events.push_back(Arc::new(event));
                        while events.len() > max_events {
                            events.pop_front();
                        }
                    }
                    Err(error) => {
                        eprintln!("ignoring corrupt journal line in events.jsonl: {error}");
                    }
                }
            }
        }

        let mut screenshots = VecDeque::new();
        for event in &events {
            if let Some(id) = &event.screenshot_id
                && directory.exists(format!("screenshots/{id}.png"))
            {
                screenshots.push_back(id.clone());
            }
        }
        while screenshots.len() > max_screenshots {
            if let Some(evicted) = screenshots.pop_front() {
                let _ = directory.remove_file(format!("screenshots/{evicted}.png"));
            }
        }

        let retained_files: HashSet<String> =
            screenshots.iter().map(|id| format!("{id}.png")).collect();
        if let Ok(entries) = directory.read_dir("screenshots") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !retained_files.contains(&name) {
                    let _ = directory.remove_file(format!("screenshots/{name}"));
                }
            }
        }

        let lines_on_disk = compact_events_file(&directory, &events)?;
        let events_file = open_append(&directory)?;
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        Ok(Arc::new(Self {
            state: Mutex::new(Some(State {
                directory,
                events_file: Some(events_file),
                lines_on_disk,
                events,
                screenshots,
                next_seq,
            })),
            sender,
            max_events,
            max_screenshots,
            #[cfg(test)]
            temp_root,
        }))
    }

    #[cfg(test)]
    pub(crate) fn new(max_screenshots: usize) -> Arc<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "qbt-journal-test-{}-{}-{}",
            std::process::id(),
            now_ms(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        let store = ArtifactStore::new(&root).unwrap();
        Self::open_with_limits(
            &store,
            MAX_EVENTS.max(max_screenshots),
            max_screenshots,
            Some(root),
        )
        .unwrap()
    }

    /// Appends an event, optionally storing a PNG screenshot the event
    /// references. Sequence assignment, storage, and broadcast happen under
    /// one lock, so no subscriber can observe events out of order.
    pub(crate) fn append(
        &self,
        kind: impl Into<String>,
        payload: serde_json::Value,
        screenshot_png: Option<Vec<u8>>,
    ) -> Arc<JournalEvent> {
        let mut guard = self.state.lock().unwrap();
        let state = guard.as_mut().expect("journal state is initialized");
        let seq = state.next_seq;
        state.next_seq += 1;
        let screenshot_id = screenshot_png.and_then(|png| {
            let id = format!("shot_{}", seq);
            if let Err(error) = write_screenshot(&state.directory, &id, &png) {
                eprintln!("failed to write journal screenshot {id}: {error:#}");
                return None;
            }
            state.screenshots.push_back(id.clone());
            while state.screenshots.len() > self.max_screenshots {
                if let Some(evicted) = state.screenshots.pop_front() {
                    let _ = state
                        .directory
                        .remove_file(format!("screenshots/{evicted}.png"));
                }
            }
            Some(id)
        });
        let event = Arc::new(JournalEvent {
            seq,
            at_ms: now_ms(),
            kind: kind.into(),
            payload,
            screenshot_id,
        });
        state.events.push_back(event.clone());
        while state.events.len() > self.max_events {
            state.events.pop_front();
        }
        if let Err(error) = persist_event(state, &event, self.max_events) {
            eprintln!("failed to persist journal event {seq}: {error:#}");
        }
        // Ignore the error: no subscribers is fine.
        let _ = self.sender.send(event.clone());
        event
    }

    /// Compacts `events.jsonl` to the active window and closes the append handle.
    pub(crate) fn flush(&self) -> anyhow::Result<()> {
        let mut guard = self.state.lock().unwrap();
        let state = guard.as_mut().expect("journal state is initialized");
        state.events_file = None;
        state.lines_on_disk = compact_events_file(&state.directory, &state.events)?;
        Ok(())
    }

    /// Subscribes and snapshots atomically: the receiver sees exactly the
    /// events appended after the returned snapshot, with no gap and no
    /// duplicate.
    pub(crate) fn subscribe_with_snapshot(
        &self,
    ) -> (
        broadcast::Receiver<Arc<JournalEvent>>,
        Vec<Arc<JournalEvent>>,
    ) {
        let guard = self.state.lock().unwrap();
        let state = guard.as_ref().expect("journal state is initialized");
        (
            self.sender.subscribe(),
            state.events.iter().cloned().collect(),
        )
    }

    /// The most recently captured screenshot still in the buffer: what the
    /// agent just saw and is marking.
    pub(crate) fn latest_screenshot(&self) -> Option<(String, Arc<Vec<u8>>)> {
        let guard = self.state.lock().unwrap();
        let state = guard.as_ref().expect("journal state is initialized");
        let id = state.screenshots.back()?;
        let bytes = state
            .directory
            .read(format!("screenshots/{id}.png"))
            .ok()
            .map(Arc::new)?;
        Some((id.clone(), bytes))
    }

    pub(crate) fn screenshot(&self, id: &str) -> Option<Arc<Vec<u8>>> {
        let guard = self.state.lock().unwrap();
        let state = guard.as_ref().expect("journal state is initialized");
        if !state.screenshots.iter().any(|stored_id| stored_id == id) {
            return None;
        }
        state
            .directory
            .read(format!("screenshots/{id}.png"))
            .ok()
            .map(Arc::new)
    }

    /// Snapshots an inclusive screenshot range in journal order. Both boundary
    /// ids must still be retained, so a caller never receives a partial range
    /// after an eviction.
    pub(crate) fn screenshot_range(
        &self,
        from: &str,
        to: &str,
    ) -> anyhow::Result<Vec<(String, Arc<Vec<u8>>)>> {
        let guard = self.state.lock().unwrap();
        let state = guard.as_ref().expect("journal state is initialized");
        let start = state
            .screenshots
            .iter()
            .position(|id| id == from)
            .ok_or_else(|| anyhow::anyhow!("screenshot evicted: {from}"))?;
        let end = state
            .screenshots
            .iter()
            .position(|id| id == to)
            .ok_or_else(|| anyhow::anyhow!("screenshot evicted: {to}"))?;
        anyhow::ensure!(start <= end, "screenshot range is not in journal order");
        let mut results = Vec::with_capacity(end - start + 1);
        for id in state.screenshots.range(start..=end) {
            let bytes = state
                .directory
                .read(format!("screenshots/{id}.png"))
                .with_context(|| format!("failed to read retained screenshot {id}"))?;
            results.push((id.clone(), Arc::new(bytes)));
        }
        Ok(results)
    }
}

#[cfg(test)]
impl Drop for Journal {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.state.lock() {
            let _ = guard.take();
        }
        if let Some(root) = self.temp_root.take() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn write_screenshot(directory: &Dir, id: &str, png: &[u8]) -> anyhow::Result<()> {
    let mut file = directory.create(format!("screenshots/{id}.png"))?;
    file.write_all(png)?;
    file.sync_data()?;
    Ok(())
}

fn open_append(directory: &Dir) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    Ok(directory.open_with("events.jsonl", &options)?)
}

fn compact_events_file(
    directory: &Dir,
    events: &VecDeque<Arc<JournalEvent>>,
) -> anyhow::Result<usize> {
    let mut temp = directory.create("events.tmp")?;
    for event in events {
        let mut line = serde_json::to_vec(&**event)?;
        line.push(b'\n');
        temp.write_all(&line)?;
    }
    temp.sync_data()?;
    drop(temp);
    directory.rename("events.tmp", directory, "events.jsonl")?;
    Ok(events.len())
}

fn persist_event(state: &mut State, event: &JournalEvent, max_events: usize) -> anyhow::Result<()> {
    if state.lines_on_disk >= max_events.saturating_mul(2) {
        state.events_file = None;
        state.lines_on_disk = compact_events_file(&state.directory, &state.events)?;
        state.events_file = Some(open_append(&state.directory)?);
        return Ok(());
    }
    if state.events_file.is_none() {
        state.events_file = Some(open_append(&state.directory)?);
    }
    let file = state.events_file.as_mut().unwrap();
    let mut line = serde_json::to_vec(event)?;
    line.push(b'\n');
    file.write_all(&line)?;
    file.flush()?;
    state.lines_on_disk += 1;
    Ok(())
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn snapshot_then_stream_is_gapless() {
        let journal = Journal::new(DEFAULT_MAX_SCREENSHOTS);
        journal.append("action", serde_json::json!({"n": 1}), None);
        let (mut receiver, snapshot) = journal.subscribe_with_snapshot();
        journal.append("action", serde_json::json!({"n": 2}), None);

        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].seq, 1);
        let streamed = receiver.recv().await.unwrap();
        assert_eq!(streamed.seq, 2);
    }

    #[tokio::test]
    async fn stores_and_serves_screenshots() {
        let journal = Journal::new(DEFAULT_MAX_SCREENSHOTS);
        let event = journal.append("action", serde_json::json!({}), Some(vec![1, 2, 3]));
        let id = event.screenshot_id.clone().unwrap();
        assert_eq!(*journal.screenshot(&id).unwrap(), vec![1, 2, 3]);
        assert!(journal.screenshot("shot_999").is_none());
    }

    #[test]
    fn screenshot_retention_evicts_oldest_and_ranges_follow_journal_order() {
        let journal = Journal::new(2);
        let first = journal.append("action", serde_json::json!({}), Some(vec![1]));
        journal.append("no-image", serde_json::json!({}), None);
        let second = journal.append("action", serde_json::json!({}), Some(vec![2]));
        let third = journal.append("action", serde_json::json!({}), Some(vec![3]));

        assert!(
            journal
                .screenshot(first.screenshot_id.as_deref().unwrap())
                .is_none()
        );
        let range = journal
            .screenshot_range(
                second.screenshot_id.as_deref().unwrap(),
                third.screenshot_id.as_deref().unwrap(),
            )
            .unwrap();
        assert_eq!(
            range.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["shot_3", "shot_4"]
        );
        assert_eq!(
            range.iter().map(|(_, png)| png[0]).collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(
            journal
                .screenshot_range("shot_1", "shot_4")
                .unwrap_err()
                .to_string(),
            "screenshot evicted: shot_1",
        );
        assert_eq!(
            journal
                .screenshot_range("shot_4", "shot_3")
                .unwrap_err()
                .to_string(),
            "screenshot range is not in journal order",
        );
    }

    #[test]
    fn event_retention_keeps_only_the_newest_events() {
        let journal = Journal::new(DEFAULT_MAX_SCREENSHOTS);
        for n in 0..(MAX_EVENTS + 2) {
            journal.append("event", serde_json::json!({ "n": n }), None);
        }

        let (_, snapshot) = journal.subscribe_with_snapshot();
        assert_eq!(snapshot.len(), MAX_EVENTS);
        assert_eq!(snapshot.first().unwrap().seq, 3);
        assert_eq!(snapshot.last().unwrap().seq, (MAX_EVENTS + 2) as u64);
    }

    #[test]
    fn disk_journal_evicts_old_files_compacts_events_and_resumes_across_restarts() {
        let root = std::env::temp_dir().join(format!(
            "qbt-journal-persist-{}-{}",
            std::process::id(),
            now_ms(),
        ));
        let store = ArtifactStore::new(&root).unwrap();
        {
            let journal = Journal::open_with_limits(&store, 3, 2, None).unwrap();
            journal.append("action", serde_json::json!({"step": 1}), Some(vec![10]));
            journal.append("action", serde_json::json!({"step": 2}), Some(vec![20]));
            journal.append("action", serde_json::json!({"step": 3}), Some(vec![30]));
            journal.append("note", serde_json::json!({"step": 4}), None);
            journal.flush().unwrap();
        }

        let journal_dir = root.join("journal");
        assert!(!journal_dir.join("screenshots/shot_1.png").exists());
        assert!(journal_dir.join("screenshots/shot_2.png").exists());
        assert!(journal_dir.join("screenshots/shot_3.png").exists());

        // Simulate an orphan screenshot and a partial trailing line before reopening.
        std::fs::write(journal_dir.join("screenshots/shot_99.png"), b"orphan").unwrap();
        std::fs::write(journal_dir.join("events.tmp"), b"stale").unwrap();
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(journal_dir.join("events.jsonl"))
                .unwrap();
            f.write_all(b"{\"seq\":999,\"incomplete").unwrap();
        }

        {
            let reopened = Journal::open_with_limits(&store, 3, 2, None).unwrap();
            assert!(!journal_dir.join("screenshots/shot_99.png").exists());
            assert!(!journal_dir.join("events.tmp").exists());

            let (_, snapshot) = reopened.subscribe_with_snapshot();
            let seqs: Vec<u64> = snapshot.iter().map(|e| e.seq).collect();
            assert_eq!(seqs, vec![2, 3, 4]);
            assert_eq!(*reopened.screenshot("shot_2").unwrap(), vec![20]);
            assert_eq!(*reopened.screenshot("shot_3").unwrap(), vec![30]);

            let next = reopened.append("after_restart", serde_json::json!({}), Some(vec![50]));
            assert_eq!(next.seq, 5);
            assert_eq!(next.screenshot_id.as_deref(), Some("shot_5"));
            assert!(!journal_dir.join("screenshots/shot_2.png").exists());
            assert!(journal_dir.join("screenshots/shot_3.png").exists());
            assert!(journal_dir.join("screenshots/shot_5.png").exists());
            reopened.flush().unwrap();
        }

        let lines: Vec<JournalEvent> = std::fs::read_to_string(journal_dir.join("events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            lines.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );

        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
