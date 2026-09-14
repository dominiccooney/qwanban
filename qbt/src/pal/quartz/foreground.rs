use super::Observation;

pub(super) fn sample() -> Option<Observation<()>> {
    // Quartz's window list is z-order, not authoritative foreground focus.
    // The current PAL has no AppKit/Accessibility foreground-window API; do not
    // guess from the first visible window or an application display name.
    None
}
