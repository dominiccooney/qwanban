use windows::Win32::Foundation::{ERROR_SUCCESS, GetLastError, HWND, SetLastError};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
};
use windows::core::{Owned, PWSTR};

use super::{ForegroundWindow, Observation, observe};

#[derive(Debug, PartialEq, Eq)]
pub(super) struct WindowIdentity {
    hwnd: HWND,
    process_id: u32,
    thread_id: u32,
}

fn identity() -> Option<WindowIdentity> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }
        let mut process_id = 0;
        let thread_id = GetWindowThreadProcessId(hwnd, Some(&mut process_id));
        if process_id == 0 || thread_id == 0 {
            return None;
        }
        Some(WindowIdentity {
            hwnd,
            process_id,
            thread_id,
        })
    }
}

pub(super) fn sample() -> Option<Observation<WindowIdentity>> {
    observe(identity, |window| ForegroundWindow {
        executable: executable(window.process_id),
        title: title(window.hwnd),
    })
}

fn executable(process_id: u32) -> Option<String> {
    unsafe {
        // Limited query access works across bitness without reading process
        // memory. RAII closes the handle on every return, including errors.
        let process =
            Owned::new(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id).ok()?);
        let mut buffer = vec![0u16; 32768];
        let mut length = buffer.len() as u32;
        QueryFullProcessImageNameW(
            *process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
        .ok()?;
        if length == 0 {
            return None;
        }
        String::from_utf16(buffer.get(..length as usize)?).ok()
    }
}

fn title(hwnd: HWND) -> Option<String> {
    // A fixed bound avoids a separate length read racing the text read. Reject
    // possible truncation rather than presenting a partial caption as complete.
    let mut buffer = vec![0u16; 32768];
    unsafe {
        SetLastError(ERROR_SUCCESS);
        let length = GetWindowTextW(hwnd, &mut buffer);
        if length < 0
            || (length == 0 && GetLastError() != ERROR_SUCCESS)
            || length as usize >= buffer.len() - 1
        {
            return None;
        }
        String::from_utf16(&buffer[..length as usize]).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_os_executable_path_and_rejects_an_invalid_process() {
        let path = executable(std::process::id()).unwrap();
        assert_eq!(
            std::path::PathBuf::from(path),
            std::env::current_exe().unwrap()
        );
        assert!(executable(0).is_none());
        assert!(title(HWND::default()).is_none());
    }

    #[test]
    fn samples_native_foreground_without_activating_a_window() {
        let observed = sample();
        // A headless, secure, or changing desktop may honestly have no sample.
        // Print the real observation for interactive test runs without requiring
        // a particular user's app to be foreground or changing their GUI state.
        eprintln!("native foreground: {observed:?}");
        if let Some(observed) = observed {
            assert!(!observed.identity.hwnd.is_invalid());
            assert_ne!(observed.identity.process_id, 0);
            if let Some(path) = observed.window.executable {
                assert!(std::path::Path::new(&path).is_absolute());
            }
        }
    }
}
