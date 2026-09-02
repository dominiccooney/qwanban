//! Scratch experiment (not a keeper test): does SendInput work from a thread
//! bound to a non-input desktop?
//!
//! Run with:
//!   cargo test -p qbt --test desktop_sendinput_probe -- --nocapture
//!
//! The probe injects only a zero-pixel relative mouse move and never calls
//! SwitchDesktop, so the interactive session is unaffected.
#![cfg(windows)]

use std::thread;
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, GetLastError};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, CreateDesktopW, DESKTOP_CONTROL_FLAGS, DESKTOP_CREATEMENU,
    DESKTOP_CREATEWINDOW, DESKTOP_DELETE, DESKTOP_ENUMERATE, DESKTOP_HOOKCONTROL,
    DESKTOP_JOURNALPLAYBACK, DESKTOP_JOURNALRECORD, DESKTOP_READOBJECTS,
    DESKTOP_READ_CONTROL, DESKTOP_SWITCHDESKTOP, DESKTOP_WRITEOBJECTS, HDESK,
    SetThreadDesktop,
};
use windows::core::{PCWSTR, w};

fn desktop_access_all() -> u32 {
    DESKTOP_READOBJECTS.0
        | DESKTOP_CREATEWINDOW.0
        | DESKTOP_CREATEMENU.0
        | DESKTOP_HOOKCONTROL.0
        | DESKTOP_JOURNALRECORD.0
        | DESKTOP_JOURNALPLAYBACK.0
        | DESKTOP_ENUMERATE.0
        | DESKTOP_WRITEOBJECTS.0
        | DESKTOP_DELETE.0
        | DESKTOP_SWITCHDESKTOP.0
        | DESKTOP_READ_CONTROL.0
}

// A zero-pixel relative mouse move: harmless by construction.
fn zero_move_input() -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_zero_move() -> (u32, u32) {
    unsafe {
        let inserted = SendInput(&[zero_move_input()], size_of::<INPUT>() as i32);
        (inserted, GetLastError().0)
    }
}

#[test]
fn sendinput_from_non_input_desktop() {
    // Control: this thread is on the default desktop, which is the input
    // desktop in an interactive session. Injection should succeed.
    let (inserted, gle) = send_zero_move();
    println!("control  (input desktop):     inserted={inserted} gle={gle}");
    assert_eq!(inserted, 1, "control SendInput on the input desktop should work");

    // Probe: a fresh thread bound via SetThreadDesktop to a newly created
    // desktop in the same window station.
    let desktop_name = w!("qbt-sendinput-probe");
    let desk = unsafe {
        CreateDesktopW(
            desktop_name,
            PCWSTR::null(),
            None,
            DESKTOP_CONTROL_FLAGS(0),
            desktop_access_all(),
            None,
        )
        .expect("CreateDesktopW")
    };

    let (probe_inserted, probe_gle) = {
        let (tx, rx) = std::sync::mpsc::channel();
        // HDESK is a raw pointer, so send it across the thread as usize.
        let desk_addr = desk.0 as usize;
        thread::spawn(move || {
            unsafe {
                SetThreadDesktop(HDESK(desk_addr as *mut core::ffi::c_void))
                    .expect("SetThreadDesktop");
                let _ = tx.send(send_zero_move());
            }
        })
        .join()
        .expect("probe thread panicked");
        rx.recv().expect("probe thread did not report")
    };

    println!("probe    (non-input desktop): inserted={probe_inserted} gle={probe_gle}");
    if probe_inserted == 0 && probe_gle == ERROR_ACCESS_DENIED.0 {
        println!("RESULT: SendInput FAILS on a non-input desktop (ERROR_ACCESS_DENIED).");
        println!("        SetThreadDesktop does not unlock input injection.");
    } else if probe_inserted == 1 {
        println!("RESULT: SendInput SUCCEEDED on the non-input desktop.");
        println!("        Events would still be routed to the input desktop's queue.");
    } else {
        println!("RESULT: unexpected — inspect inserted/gle above.");
    }

    unsafe { CloseDesktop(desk).expect("CloseDesktop") };
}
