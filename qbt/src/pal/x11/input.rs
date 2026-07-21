use std::time::Duration;
use anyhow::Context;
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConnectionExt as _, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::CURRENT_TIME;
use xkeysym::{key, Keysym};

use crate::input::Key;
use crate::computer_use::ScrollDirection;
use crate::pal::x11_connection::{connection, keycode_for_keysym, X11Connection};

// See libX11 X11/keysymdef.h. Named keys map onto their X11 keysym; typed and chord-literal
// characters both resolve through Keysym::from_char, since XTEST has no direct analog of
// Windows' KEYEVENTF_UNICODE and the remapping trick in keycode_for_keysym() covers both
// cases equally well.
fn keysym_of_key(key: Key) -> Keysym {
    match key {
        Key::Alt => Keysym::from(key::Alt_L),
        Key::BackSpace => Keysym::from(key::BackSpace),
        Key::Ctrl => Keysym::from(key::Control_L),
        Key::Delete => Keysym::from(key::Delete),
        Key::Down => Keysym::from(key::Down),
        Key::End => Keysym::from(key::End),
        Key::Escape => Keysym::from(key::Escape),
        Key::F(n) => Keysym::from(key::F1 + (n as u32 - 1)),
        Key::Home => Keysym::from(key::Home),
        Key::Left => Keysym::from(key::Left),
        Key::PageDown => Keysym::from(key::Page_Down),
        Key::PageUp => Keysym::from(key::Page_Up),
        Key::Return => Keysym::from(key::Return),
        Key::Right => Keysym::from(key::Right),
        Key::Shift => Keysym::from(key::Shift_L),
        Key::Super => Keysym::from(key::Super_L),
        Key::Tab => Keysym::from(key::Tab),
        Key::Up => Keysym::from(key::Up),
        Key::Typed(ch) | Key::Literal(ch) => Keysym::from_char(ch),
    }
}

fn send_fake_input(x11: &X11Connection, type_: u8, detail: u8, root_x: i16, root_y: i16) -> anyhow::Result<()> {
    x11.conn.xtest_fake_input(type_, detail, CURRENT_TIME, x11.screen.root, root_x, root_y, 0)?
        .check()
        .context("sending a synthetic input event")?;
    x11.conn.flush()?;
    Ok(())
}

pub(crate) fn send_key_down(key: Key) -> anyhow::Result<()> {
    let x11 = connection()?;
    let keycode = keycode_for_keysym(x11, keysym_of_key(key))?;
    send_fake_input(x11, KEY_PRESS_EVENT, keycode, 0, 0)
}

pub(crate) fn send_key_up(key: Key) -> anyhow::Result<()> {
    let x11 = connection()?;
    let keycode = keycode_for_keysym(x11, keysym_of_key(key))?;
    send_fake_input(x11, KEY_RELEASE_EVENT, keycode, 0, 0)
}

pub(crate) fn cursor_position() -> anyhow::Result<(usize, usize)> {
    let x11 = connection()?;
    let reply = x11.conn.query_pointer(x11.screen.root)?.reply().context("querying the pointer position")?;
    Ok((reply.root_x as usize, reply.root_y as usize))
}

pub(crate) async fn mouse_move_to((end_x, end_y): (i32, i32)) -> anyhow::Result<()> {
    let x11 = connection()?;
    let (start_x, start_y) = cursor_position()?;
    let start_x = start_x as i32;
    let start_y = start_y as i32;

    let distance = (((start_x - end_x).pow(2) + (start_y - end_y).pow(2)) as f64).sqrt();
    let steps = (distance / 20.0).ceil() as usize;

    for i in 0..steps {
        let t: f64 = -6.0 + 12.0 * i as f64 / steps as f64;
        let sigma = 1.0 / (1.0 + (-t).exp());
        let x = start_x + ((end_x - start_x) as f64 * sigma) as i32;
        let y = start_y + ((end_y - start_y) as f64 * sigma) as i32;
        // detail == 0 selects absolute positioning for XTEST MotionNotify events.
        send_fake_input(x11, MOTION_NOTIFY_EVENT, 0, x as i16, y as i16)?;
        tokio::time::sleep(Duration::from_millis(4)).await;
    }

    send_fake_input(x11, MOTION_NOTIFY_EVENT, 0, end_x as i16, end_y as i16)
}

#[derive(Copy, Clone)]
pub(crate) enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    // X11 button numbers, as used by both core ButtonPress events and XTEST FakeInput.
    fn button_number(self) -> u8 {
        match self {
            MouseButton::Left => 1,
            MouseButton::Middle => 2,
            MouseButton::Right => 3,
        }
    }
}

pub(crate) async fn mouse_down(button: MouseButton) -> anyhow::Result<()> {
    send_fake_input(connection()?, BUTTON_PRESS_EVENT, button.button_number(), 0, 0)
}

pub(crate) async fn mouse_up(button: MouseButton) -> anyhow::Result<()> {
    send_fake_input(connection()?, BUTTON_RELEASE_EVENT, button.button_number(), 0, 0)
}

pub(crate) async fn mouse_scroll(clicks: &f64, direction: &ScrollDirection) -> anyhow::Result<()> {
    // The conventional X11/evdev scroll wheel button numbers: 4/5 for vertical, 6/7 for
    // horizontal. There is no XTEST analog of Windows' fractional mouse wheel delta, so each
    // "click" is a discrete button press/release, as xdotool and similar tools also do.
    let button = match direction {
        ScrollDirection::Up => 4,
        ScrollDirection::Down => 5,
        ScrollDirection::Left => 6,
        ScrollDirection::Right => 7,
    };
    let x11 = connection()?;
    for _ in 0..clicks.round().max(1.0) as u32 {
        send_fake_input(x11, BUTTON_PRESS_EVENT, button, 0, 0)?;
        send_fake_input(x11, BUTTON_RELEASE_EVENT, button, 0, 0)?;
    }
    Ok(())
}
