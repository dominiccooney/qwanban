#[cfg(target_os = "windows")]
#[path = "pal/windows/screen.rs"]
mod os_screen_impl;

#[cfg(target_os = "windows")]
#[path = "pal/windows/input.rs"]
mod os_input_impl;

#[cfg(target_os = "linux")]
#[path = "pal/x11/connection.rs"]
mod x11_connection;

#[cfg(target_os = "linux")]
#[path = "pal/x11/screen.rs"]
mod os_screen_impl;

#[cfg(target_os = "linux")]
#[path = "pal/x11/input.rs"]
mod os_input_impl;

#[cfg(target_os = "macos")]
#[path = "pal/quartz/screen.rs"]
mod os_screen_impl;

#[cfg(target_os = "macos")]
#[path = "pal/quartz/input.rs"]
mod os_input_impl;

pub(crate) use os_input_impl::*;
pub(crate) use os_screen_impl::*;

/// Captures one screenshot. Windows desktop GDI resources can become invalid
/// when the interactive desktop is replaced (for example after reconnecting a
/// remote session). A failed capture drops the complete sampler before one
/// bounded retry reacquires the desktop window, device contexts, and bitmap as
/// one consistent resource set.
pub(crate) fn screenshot() -> anyhow::Result<ScreenshotImage> {
    #[cfg(target_os = "windows")]
    {
        return retry_read_once(|| ScreenSampler::new()?.screenshot());
    }

    #[cfg(not(target_os = "windows"))]
    {
        ScreenSampler::new()?.screenshot()
    }
}

#[cfg(target_os = "windows")]
fn retry_read_once<T>(mut operation: impl FnMut() -> anyhow::Result<T>) -> anyhow::Result<T> {
    match operation() {
        Ok(value) => Ok(value),
        Err(first_error) => operation().map_err(|second_error| {
            anyhow::anyhow!(
                "screen capture failed after reacquiring the Windows desktop resource: {second_error}; first attempt: {first_error}"
            )
        }),
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::retry_read_once;

    #[test]
    fn failed_read_reacquires_once() {
        let mut attempts = 0;
        let value = retry_read_once(|| {
            attempts += 1;
            if attempts == 1 {
                anyhow::bail!("stale desktop handle")
            }
            Ok(42)
        })
        .unwrap();

        assert_eq!(value, 42);
        assert_eq!(attempts, 2);
    }

    #[test]
    fn repeated_failure_is_bounded_and_preserves_both_errors() {
        let mut attempts = 0;
        let error = retry_read_once::<()>(|| {
            attempts += 1;
            anyhow::bail!("failure {attempts}")
        })
        .unwrap_err()
        .to_string();

        assert_eq!(attempts, 2);
        assert!(error.contains("failure 1"));
        assert!(error.contains("failure 2"));
    }
}
