use serde::Serialize;

use super::ScreenshotImage;

#[cfg(target_os = "windows")]
#[path = "windows/foreground.rs"]
mod os_impl;
#[cfg(target_os = "linux")]
#[path = "x11/foreground.rs"]
mod os_impl;
#[cfg(target_os = "macos")]
#[path = "quartz/foreground.rs"]
mod os_impl;

/// OS-observed foreground window, not editable-control focus or authority to
/// send input. Titles are untrusted application content. A missing field is
/// unavailable; an empty title is a known untitled window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ForegroundWindow {
    pub(crate) executable: Option<String>,
    pub(crate) title: Option<String>,
}

/// Pixels and their foreground observation travel together through cropping,
/// guard refusal, response encoding, and journaling. None also covers platforms
/// without a supported foreground-window API; metadata never fails a capture.
pub(crate) struct CapturedScreenshot {
    pub(crate) image: ScreenshotImage,
    pub(crate) foreground_window: Option<ForegroundWindow>,
}

#[derive(Debug, PartialEq, Eq)]
struct Observation<I> {
    identity: I,
    window: ForegroundWindow,
}

/// Read both fields for one identity, then reject a detected foreground change.
#[cfg(any(target_os = "windows", target_os = "linux", test))]
fn observe<I: Eq>(
    mut identity: impl FnMut() -> Option<I>,
    fields: impl FnOnce(&I) -> ForegroundWindow,
) -> Option<Observation<I>> {
    let before = identity()?;
    let window = fields(&before);
    let after = identity()?;
    (before == after).then_some(Observation {
        identity: before,
        window,
    })
}

pub(super) fn capture(
    pixels: impl FnOnce() -> anyhow::Result<ScreenshotImage>,
) -> anyhow::Result<CapturedScreenshot> {
    capture_with_observation(pixels, os_impl::sample)
}

fn capture_with_observation<I: Eq>(
    pixels: impl FnOnce() -> anyhow::Result<ScreenshotImage>,
    mut sample: impl FnMut() -> Option<Observation<I>>,
) -> anyhow::Result<CapturedScreenshot> {
    // One synchronous capture bracket is the consistency boundary: compare the
    // identity AND fields on both sides of pixel capture, before PNG encoding.
    // OS reads are not atomic; an away-and-back transition can go undetected.
    let before = sample();
    let image = pixels()?;
    let after = sample();
    let foreground_window = match (before, after) {
        (Some(before), Some(after)) if before == after => Some(after.window),
        _ => None,
    };
    Ok(CapturedScreenshot {
        image,
        foreground_window,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn fields() -> ForegroundWindow {
        ForegroundWindow {
            executable: Some("C:\\app.exe".into()),
            title: Some("Document".into()),
        }
    }

    fn observation(identity: u32) -> Option<Observation<u32>> {
        Some(Observation {
            identity,
            window: fields(),
        })
    }

    #[test]
    fn field_reads_recheck_the_same_identity() {
        let current = Cell::new(Some(1));
        let stable = observe(
            || current.get(),
            |id| {
                assert_eq!(*id, 1);
                fields()
            },
        )
        .unwrap();
        assert_eq!(stable.window, fields());
        for changed in [Some(2), None] {
            current.set(Some(1));
            assert!(
                observe(
                    || current.get(),
                    |_| {
                        current.set(changed);
                        fields()
                    }
                )
                .is_none()
            );
        }
        assert!(observe(|| None::<u32>, |_| panic!("no foreground to read")).is_none());
    }

    #[test]
    fn capture_rejects_changed_identity_or_fields_but_keeps_pixels() {
        let mut changed_title = observation(1).unwrap();
        changed_title.window.title = Some("Another document".into());
        let mut changed_executable = observation(1).unwrap();
        changed_executable.window.executable = None;
        for after in [
            observation(2),
            Some(changed_title),
            Some(changed_executable),
            None,
        ] {
            let mut samples = [observation(1), after].into_iter();
            let image = ScreenshotImage::new(2, 3);
            let result =
                capture_with_observation(|| Ok(image.clone()), || samples.next().unwrap()).unwrap();
            assert!(result.foreground_window.is_none());
            assert_eq!(result.image, image);
        }
    }

    #[test]
    fn capture_samples_around_pixels_and_preserves_partial_unknowns() {
        for window in [
            fields(),
            ForegroundWindow {
                executable: None,
                title: Some(String::new()),
            },
            ForegroundWindow {
                executable: None,
                title: None,
            },
        ] {
            let order = Cell::new(0);
            let result = capture_with_observation(
                || {
                    assert_eq!(order.replace(2), 1);
                    Ok(ScreenshotImage::new(1, 1))
                },
                || {
                    let step = order.get();
                    assert!(step == 0 || step == 2);
                    order.set(step + 1);
                    Some(Observation {
                        identity: 1,
                        window: window.clone(),
                    })
                },
            )
            .unwrap();
            assert_eq!(order.get(), 3);
            assert_eq!(result.foreground_window, Some(window));
        }
        for mut samples in [[None, None].into_iter(), [None, observation(1)].into_iter()] {
            assert!(
                capture_with_observation(
                    || Ok(ScreenshotImage::new(1, 1)),
                    || samples.next().unwrap()
                )
                .unwrap()
                .foreground_window
                .is_none()
            );
        }
        assert!(
            capture_with_observation(|| anyhow::bail!("capture failed"), || observation(1))
                .is_err()
        );
    }
}
