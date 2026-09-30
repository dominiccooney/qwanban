//! In-memory journal of everything observable about this host.
//!
//! The journal is the single source of truth the observatory reads. Two
//! producers write to it: the agent server journals computer actions and
//! screenshots as it executes them (it is the only component that sees them
//! all, in execution order), and the agent publishes its own events
//! (transcripts, coordinator status changes) over the same socket. Observers are
//! pure subscribers: they take a snapshot plus a live stream, both in one
//! total order (`seq`).

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

const MAX_EVENTS: usize = 500;
const BROADCAST_CAPACITY: usize = 256;

pub(crate) const DEFAULT_MAX_SCREENSHOTS: usize = 200;

#[derive(Serialize, Debug)]
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
    events: VecDeque<Arc<JournalEvent>>,
    screenshots: VecDeque<(String, Arc<Vec<u8>>)>,
    next_seq: u64,
}

pub(crate) struct Journal {
    state: Mutex<State>,
    sender: broadcast::Sender<Arc<JournalEvent>>,
    max_screenshots: usize,
}

impl Journal {
    pub(crate) fn new(max_screenshots: usize) -> Arc<Self> {
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        Arc::new(Self {
            state: Mutex::new(State {
                events: VecDeque::new(),
                screenshots: VecDeque::new(),
                next_seq: 1,
            }),
            sender,
            max_screenshots,
        })
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
        let mut state = self.state.lock().unwrap();
        let seq = state.next_seq;
        state.next_seq += 1;
        let screenshot_id = screenshot_png.map(|png| {
            let id = format!("shot_{}", seq);
            state.screenshots.push_back((id.clone(), Arc::new(png)));
            while state.screenshots.len() > self.max_screenshots {
                state.screenshots.pop_front();
            }
            id
        });
        let event = Arc::new(JournalEvent {
            seq,
            at_ms: now_ms(),
            kind: kind.into(),
            payload,
            screenshot_id,
        });
        state.events.push_back(event.clone());
        while state.events.len() > MAX_EVENTS {
            state.events.pop_front();
        }
        // Ignore the error: no subscribers is fine.
        let _ = self.sender.send(event.clone());
        event
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
        let state = self.state.lock().unwrap();
        (
            self.sender.subscribe(),
            state.events.iter().cloned().collect(),
        )
    }

    pub(crate) fn screenshot(&self, id: &str) -> Option<Arc<Vec<u8>>> {
        let state = self.state.lock().unwrap();
        state
            .screenshots
            .iter()
            .find(|(stored_id, _)| stored_id == id)
            .map(|(_, png)| png.clone())
    }

    /// Snapshots an inclusive screenshot range in journal order. Both boundary
    /// ids must still be retained, so a caller never receives a partial range
    /// after an eviction.
    pub(crate) fn screenshot_range(
        &self,
        from: &str,
        to: &str,
    ) -> anyhow::Result<Vec<(String, Arc<Vec<u8>>)>> {
        let state = self.state.lock().unwrap();
        let start = state
            .screenshots
            .iter()
            .position(|(id, _)| id == from)
            .ok_or_else(|| anyhow::anyhow!("screenshot evicted: {from}"))?;
        let end = state
            .screenshots
            .iter()
            .position(|(id, _)| id == to)
            .ok_or_else(|| anyhow::anyhow!("screenshot evicted: {to}"))?;
        anyhow::ensure!(start <= end, "screenshot range is not in journal order");
        Ok(state
            .screenshots
            .range(start..=end)
            .map(|(id, png)| (id.clone(), png.clone()))
            .collect())
    }
}

fn now_ms() -> u64 {
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
}
