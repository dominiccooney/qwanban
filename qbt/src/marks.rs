//! Marks: screenshots the agent chose to keep, and flipbooks built from them.
//!
//! A mark is the screenshot the agent was looking at plus a short label. It
//! is written to disk under the artifact root the moment it is taken, so a
//! backend restart or journal eviction cannot lose it. Pending marks sit in
//! `marks/pending/` named by timestamp; their order is the file order, not
//! the journal counter, so numbering survives restarts. A flipbook moves the
//! marks it used into `flipbooks/<name>/frames/` beside the encoded WebP.

use crate::artifacts::ArtifactStore;
use crate::flipbook;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const PENDING_DIR: &str = "marks/pending";
const FLIPBOOKS_DIR: &str = "flipbooks";
const MAX_LABEL_CHARS: usize = 200;
const MAX_SLUG_CHARS: usize = 48;
pub(crate) const FLIPBOOK_FPS: f64 = 1.0;

/// Sidecar written beside each mark's PNG.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct MarkRecord {
    label: String,
    at_ms: u64,
    screenshot_id: String,
}

/// One pending mark as listed to the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mark {
    /// 1-based position in listing order.
    pub number: usize,
    pub label: String,
    pub at_ms: u64,
    /// Absolute path of the full-resolution PNG.
    pub png: PathBuf,
    stem: String,
}

/// Which pending marks an action applies to. Omitted means all.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub(crate) enum MarkSelector {
    Numbers(Vec<usize>),
    Text(String),
}

impl MarkSelector {
    /// Resolves to 1-based mark numbers in ascending order, each validated
    /// against `count`.
    fn resolve(&self, count: usize) -> anyhow::Result<Vec<usize>> {
        let mut numbers = match self {
            MarkSelector::Numbers(numbers) => numbers.clone(),
            MarkSelector::Text(text) => parse_selector_text(text, count)?,
        };
        anyhow::ensure!(!numbers.is_empty(), "marks selector is empty");
        numbers.sort_unstable();
        numbers.dedup();
        for number in &numbers {
            anyhow::ensure!(
                (1..=count).contains(number),
                "mark {number} does not exist; there are {count} pending marks"
            );
        }
        Ok(numbers)
    }
}

/// Accepts a single number, a range "a-b", an open range "a-" (to the last
/// mark) or "-b" (from the first), or a comma list of those.
fn parse_selector_text(text: &str, count: usize) -> anyhow::Result<Vec<usize>> {
    let text = text.trim();
    let mut numbers = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        let invalid = || {
            anyhow::anyhow!(
                "invalid marks selector {text:?}; use e.g. [1,3], \"2-5\", \"3-\", or omit for all"
            )
        };
        if let Some((start, end)) = part.split_once('-') {
            let (start, end) = (start.trim(), end.trim());
            let start: usize = if start.is_empty() {
                1
            } else {
                start.parse().map_err(|_| invalid())?
            };
            let end: usize = if end.is_empty() {
                count
            } else {
                end.parse().map_err(|_| invalid())?
            };
            anyhow::ensure!(start <= end, "invalid marks range {part:?}");
            numbers.extend(start..=end);
        } else {
            numbers.push(part.parse().map_err(|_| invalid())?);
        }
    }
    Ok(numbers)
}

fn resolve(selector: Option<&MarkSelector>, count: usize) -> anyhow::Result<Vec<usize>> {
    match selector {
        None => Ok((1..=count).collect()),
        Some(MarkSelector::Text(text)) if text.trim().eq_ignore_ascii_case("all") => {
            Ok((1..=count).collect())
        }
        Some(selector) => selector.resolve(count),
    }
}

/// Lowercase ASCII letters, digits, and single hyphens; never empty.
pub(crate) fn slug(text: &str) -> String {
    let mut slug = String::new();
    let mut pending_hyphen = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_hyphen && !slug.is_empty() {
                slug.push('-');
            }
            pending_hyphen = false;
            slug.push(c.to_ascii_lowercase());
            if slug.len() >= MAX_SLUG_CHARS {
                break;
            }
        } else {
            pending_hyphen = true;
        }
    }
    if slug.is_empty() {
        "mark".to_owned()
    } else {
        slug
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Sortable, filesystem-safe stem: milliseconds since the epoch, zero padded
/// so lexical order is time order; a counter that orders marks taken inside
/// the same millisecond; then the label slug.
fn stem(at_ms: u64, sequence: usize, label: &str) -> String {
    format!("{at_ms:015}_{sequence:03}_{}", slug(label))
}

/// Saves the screenshot and its label; returns the mark as it will list.
pub(crate) fn mark(
    artifacts: &ArtifactStore,
    screenshot_id: &str,
    png: &[u8],
    label: &str,
) -> anyhow::Result<Mark> {
    let label = label.trim();
    anyhow::ensure!(!label.is_empty(), "mark requires a non-empty label");
    anyhow::ensure!(
        label.chars().count() <= MAX_LABEL_CHARS,
        "mark label must be at most {MAX_LABEL_CHARS} characters"
    );
    let pending = Path::new(PENDING_DIR);
    let at_ms = now_ms();
    let prefix = format!("{at_ms:015}_");
    let sequence = artifacts
        .list_files(pending)?
        .iter()
        .filter(|name| name.starts_with(&prefix) && name.ends_with(".png"))
        .count();
    let stem = stem(at_ms, sequence, label);
    let record = MarkRecord {
        label: label.to_owned(),
        at_ms,
        screenshot_id: screenshot_id.to_owned(),
    };
    artifacts.save(
        &pending.join(format!("{stem}.json")),
        serde_json::to_string_pretty(&record)?.as_bytes(),
    )?;
    let png = artifacts.save(&pending.join(format!("{stem}.png")), png)?;
    let number = list(artifacts)?
        .into_iter()
        .find(|mark| mark.stem == stem)
        .map(|mark| mark.number)
        .unwrap_or(0);
    Ok(Mark {
        number,
        label: record.label,
        at_ms,
        png,
        stem,
    })
}

/// Pending marks in order. A PNG without a readable sidecar is listed with
/// its file name as the label rather than hidden.
pub(crate) fn list(artifacts: &ArtifactStore) -> anyhow::Result<Vec<Mark>> {
    let pending = Path::new(PENDING_DIR);
    let mut marks = Vec::new();
    for name in artifacts.list_files(pending)? {
        let Some(stem) = name.strip_suffix(".png") else {
            continue;
        };
        let record = artifacts
            .read(&pending.join(format!("{stem}.json")))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<MarkRecord>(&bytes).ok());
        let (label, at_ms) = match record {
            Some(record) => (record.label, record.at_ms),
            None => (stem.to_owned(), 0),
        };
        marks.push(Mark {
            number: marks.len() + 1,
            label,
            at_ms,
            png: artifacts.root().join(pending).join(&name),
            stem: stem.to_owned(),
        });
    }
    Ok(marks)
}

fn remove_mark_files(artifacts: &ArtifactStore, mark: &Mark, keep_png: bool) -> anyhow::Result<()> {
    let pending = Path::new(PENDING_DIR);
    if !keep_png {
        artifacts.remove(&pending.join(format!("{}.png", mark.stem)))?;
    }
    let sidecar = pending.join(format!("{}.json", mark.stem));
    if artifacts.exists(&sidecar)? {
        artifacts.remove(&sidecar)?;
    }
    Ok(())
}

/// Deletes the selected pending marks; returns how many.
pub(crate) fn clear(
    artifacts: &ArtifactStore,
    selector: Option<&MarkSelector>,
) -> anyhow::Result<usize> {
    let marks = list(artifacts)?;
    let numbers = resolve(selector, marks.len())?;
    for number in &numbers {
        remove_mark_files(artifacts, &marks[number - 1], false)?;
    }
    Ok(numbers.len())
}

/// A frame of a finished flipbook, as recorded in `frames.json`.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FlipbookFrame {
    pub index: usize,
    pub label: String,
    pub at_ms: u64,
    pub path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct Flipbook {
    pub webp: PathBuf,
    pub frames: Vec<FlipbookFrame>,
}

/// Encodes the selected pending marks into `flipbooks/<name>/<name>.webp`
/// and moves their PNGs beside it. A reused name gets a numeric suffix.
pub(crate) fn flipbook(
    artifacts: &ArtifactStore,
    name: &str,
    selector: Option<&MarkSelector>,
) -> anyhow::Result<Flipbook> {
    let marks = list(artifacts)?;
    anyhow::ensure!(
        !marks.is_empty(),
        "no pending marks; use \"mark\" after key moments first"
    );
    let numbers = resolve(selector, marks.len())?;
    let selected: Vec<&Mark> = numbers.iter().map(|number| &marks[number - 1]).collect();

    let base = slug(name);
    let mut name = base.clone();
    let mut suffix = 1;
    let flipbooks = Path::new(FLIPBOOKS_DIR);
    while artifacts.exists(&flipbooks.join(&name).join(format!("{name}.webp")))? {
        suffix += 1;
        name = format!("{base}-{suffix}");
    }
    let directory = flipbooks.join(&name);

    let pending = Path::new(PENDING_DIR);
    let pngs = selected
        .iter()
        .map(|mark| artifacts.read(&pending.join(format!("{}.png", mark.stem))))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let captions: Vec<String> = selected.iter().map(|mark| mark.label.clone()).collect();
    let bytes = flipbook::encode(&pngs, FLIPBOOK_FPS, Some(&captions))?;
    let webp = artifacts.save(&directory.join(format!("{name}.webp")), &bytes)?;

    let width = selected.len().to_string().len().max(2);
    let mut frames = Vec::new();
    for (index, mark) in selected.iter().enumerate() {
        let index = index + 1;
        let frame_name = format!("{index:0width$}_{}.png", slug(&mark.label));
        let path = artifacts.rename(
            &pending.join(format!("{}.png", mark.stem)),
            &directory.join("frames").join(frame_name),
        )?;
        remove_mark_files(artifacts, mark, true)?;
        frames.push(FlipbookFrame {
            index,
            label: mark.label.clone(),
            at_ms: mark.at_ms,
            path,
        });
    }
    artifacts.save(
        &directory.join("frames.json"),
        serde_json::to_string_pretty(&frames)?.as_bytes(),
    )?;
    Ok(Flipbook { webp, frames })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba, RgbaImage};
    use std::io::Cursor;

    fn store(name: &str) -> (PathBuf, ArtifactStore) {
        let root = std::env::temp_dir().join(format!(
            "qbt-marks-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let store = ArtifactStore::new(&root).unwrap();
        (root, store)
    }

    fn png() -> Vec<u8> {
        let image = RgbaImage::from_pixel(8, 4, Rgba([200, 200, 200, 255]));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    fn labels(store: &ArtifactStore) -> Vec<(usize, String)> {
        list(store)
            .unwrap()
            .into_iter()
            .map(|mark| (mark.number, mark.label))
            .collect()
    }

    #[test]
    fn marks_number_in_time_order_and_survive_a_new_store() {
        let (root, store) = store("order");
        mark(&store, "shot_1", &png(), "Launcher clicked").unwrap();
        let second = mark(&store, "shot_2", &png(), "Editor open").unwrap();
        assert_eq!(second.number, 2);
        assert!(second.png.is_absolute() && second.png.exists());

        // A restarted backend opens the same root and sees the same marks.
        let reopened = ArtifactStore::new(&root).unwrap();
        assert_eq!(
            labels(&reopened),
            vec![(1, "Launcher clicked".into()), (2, "Editor open".into())]
        );
        drop(reopened);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_empty_labels() {
        let (root, store) = store("empty-label");
        assert!(mark(&store, "shot_1", &png(), "   ").is_err());
        assert!(list(&store).unwrap().is_empty());
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selector_accepts_lists_ranges_and_all() {
        assert_eq!(resolve(None, 3).unwrap(), vec![1, 2, 3]);
        assert_eq!(
            resolve(Some(&MarkSelector::Text("all".into())), 2).unwrap(),
            vec![1, 2]
        );
        assert_eq!(
            resolve(Some(&MarkSelector::Text("2-4, 1".into())), 5).unwrap(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(
            resolve(Some(&MarkSelector::Numbers(vec![3, 1, 3])), 3).unwrap(),
            vec![1, 3]
        );
        let error = resolve(Some(&MarkSelector::Numbers(vec![1, 9])), 3)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "mark 9 does not exist; there are 3 pending marks");
        assert_eq!(
            resolve(Some(&MarkSelector::Text("3-".into())), 5).unwrap(),
            vec![3, 4, 5]
        );
        assert_eq!(
            resolve(Some(&MarkSelector::Text("-2".into())), 5).unwrap(),
            vec![1, 2]
        );
        assert!(resolve(Some(&MarkSelector::Text("3-".into())), 2).is_err());
        assert!(resolve(Some(&MarkSelector::Text("x".into())), 3).is_err());
        assert!(resolve(Some(&MarkSelector::Text("5-2".into())), 9).is_err());
        assert!(resolve(Some(&MarkSelector::Numbers(vec![])), 3).is_err());
    }

    #[test]
    fn flipbook_takes_a_selection_and_leaves_the_rest_pending() {
        let (root, store) = store("flipbook");
        for label in ["one", "two", "three"] {
            mark(&store, "shot", &png(), label).unwrap();
        }
        let book = flipbook(&store, "My Run!", Some(&MarkSelector::Text("1-2".into()))).unwrap();

        assert_eq!(
            book.webp,
            root.canonicalize()
                .unwrap()
                .join("flipbooks/my-run/my-run.webp")
        );
        assert_eq!(&std::fs::read(&book.webp).unwrap()[..4], b"RIFF");
        assert_eq!(book.frames.len(), 2);
        assert!(book.frames[0].path.ends_with("frames/01_one.png"));
        assert!(book.frames[0].path.exists());
        let sidecar: serde_json::Value = serde_json::from_slice(
            &std::fs::read(book.webp.parent().unwrap().join("frames.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(sidecar[1]["label"], "two");
        assert_eq!(labels(&store), vec![(1, "three".into())]);

        // Reusing the name does not overwrite the first flipbook.
        let again = flipbook(&store, "my run", None).unwrap();
        assert!(again.webp.ends_with("my-run-2/my-run-2.webp"));
        assert!(list(&store).unwrap().is_empty());
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn flipbook_with_a_bad_selection_changes_nothing() {
        let (root, store) = store("bad-selection");
        mark(&store, "shot", &png(), "only").unwrap();
        let error = flipbook(&store, "x", Some(&MarkSelector::Numbers(vec![2])))
            .unwrap_err()
            .to_string();
        assert!(error.contains("mark 2 does not exist"));
        assert_eq!(list(&store).unwrap().len(), 1);
        assert!(!root.join("flipbooks").exists());

        let error = flipbook(&ArtifactStore::new(root.join("empty")).unwrap(), "x", None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no pending marks"));
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clear_removes_selected_marks() {
        let (root, store) = store("clear");
        for label in ["a", "b", "c"] {
            mark(&store, "shot", &png(), label).unwrap();
        }
        assert_eq!(
            clear(&store, Some(&MarkSelector::Numbers(vec![2]))).unwrap(),
            1
        );
        assert_eq!(labels(&store), vec![(1, "a".into()), (2, "c".into())]);
        assert_eq!(clear(&store, None).unwrap(), 2);
        assert!(list(&store).unwrap().is_empty());
        assert_eq!(
            std::fs::read_dir(root.join("marks/pending"))
                .unwrap()
                .count(),
            0
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn slugs_are_safe_file_names() {
        assert_eq!(
            slug("Mousepad: open, GRAPEFRUIT typed!"),
            "mousepad-open-grapefruit-typed"
        );
        assert_eq!(slug("日本語"), "mark");
        assert_eq!(slug("../etc/passwd"), "etc-passwd");
        assert!(slug(&"word ".repeat(40)).len() <= MAX_SLUG_CHARS);
    }
}
