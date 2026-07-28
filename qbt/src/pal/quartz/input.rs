use std::time::Duration;

use anyhow::{anyhow, bail};
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton, KeyCode, ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use crate::computer_use::ScrollDirection;
use crate::input::Key;

fn event_source() -> anyhow::Result<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| anyhow!("failed to create a Quartz event source"))
}

fn post_keyboard_event(key: Key, down: bool) -> anyhow::Result<()> {
    let (keycode, text) = match key {
        Key::Typed(ch) => (0, Some(ch.to_string())),
        Key::Literal(ch) => (keycode_for_literal(ch)?, None),
        Key::Alt => (KeyCode::OPTION, None),
        Key::BackSpace => (KeyCode::DELETE, None),
        Key::Ctrl => (KeyCode::CONTROL, None),
        Key::Delete => (KeyCode::FORWARD_DELETE, None),
        Key::Down => (KeyCode::DOWN_ARROW, None),
        Key::End => (KeyCode::END, None),
        Key::Escape => (KeyCode::ESCAPE, None),
        Key::F(n) => (function_keycode(n)?, None),
        Key::Home => (KeyCode::HOME, None),
        Key::Left => (KeyCode::LEFT_ARROW, None),
        Key::PageDown => (KeyCode::PAGE_DOWN, None),
        Key::PageUp => (KeyCode::PAGE_UP, None),
        Key::Return => (KeyCode::RETURN, None),
        Key::Right => (KeyCode::RIGHT_ARROW, None),
        Key::Shift => (KeyCode::SHIFT, None),
        Key::Super => (KeyCode::COMMAND, None),
        Key::Tab => (KeyCode::TAB, None),
        Key::Up => (KeyCode::UP_ARROW, None),
    };
    let event = CGEvent::new_keyboard_event(event_source()?, keycode, down)
        .map_err(|_| anyhow!("failed to create a Quartz keyboard event"))?;
    if let Some(text) = text {
        event.set_string(&text);
    }
    event.post(CGEventTapLocation::HID);
    Ok(())
}

pub(crate) fn send_key_down(key: Key) -> anyhow::Result<()> {
    post_keyboard_event(key, true)
}

pub(crate) fn send_key_up(key: Key) -> anyhow::Result<()> {
    post_keyboard_event(key, false)
}

pub(crate) fn cursor_position() -> anyhow::Result<(usize, usize)> {
    let event = CGEvent::new(event_source()?)
        .map_err(|_| anyhow!("failed to query the Quartz cursor position"))?;
    let point = points_to_pixels(event.location())?;
    Ok((point.x.max(0.0) as usize, point.y.max(0.0) as usize))
}

pub(crate) async fn mouse_move_to((end_x, end_y): (i32, i32)) -> anyhow::Result<()> {
    let (start_x, start_y) = cursor_position()?;
    let (start_x, start_y) = (start_x as i32, start_y as i32);
    let distance = (((start_x - end_x).pow(2) + (start_y - end_y).pow(2)) as f64).sqrt();
    let steps = (distance / 20.0).ceil() as usize;
    for i in 0..steps {
        let t = -6.0 + 12.0 * i as f64 / steps as f64;
        let sigma = 1.0 / (1.0 + (-t).exp());
        post_mouse_move(
            start_x + ((end_x - start_x) as f64 * sigma) as i32,
            start_y + ((end_y - start_y) as f64 * sigma) as i32,
        )?;
        tokio::time::sleep(Duration::from_millis(4)).await;
    }
    post_mouse_move(end_x, end_y)
}

fn post_mouse_move(x: i32, y: i32) -> anyhow::Result<()> {
    let event = CGEvent::new_mouse_event(
        event_source()?,
        CGEventType::MouseMoved,
        pixels_to_points(CGPoint::new(x as f64, y as f64))?,
        CGMouseButton::Left,
    )
    .map_err(|_| anyhow!("failed to create a Quartz mouse movement event"))?;
    event.post(CGEventTapLocation::HID);
    Ok(())
}

#[derive(Copy, Clone)]
pub(crate) enum MouseButton {
    Left,
    Right,
    Middle,
}

pub(crate) async fn mouse_down(button: MouseButton) -> anyhow::Result<()> {
    post_mouse_button(button, true)
}

pub(crate) async fn mouse_up(button: MouseButton) -> anyhow::Result<()> {
    post_mouse_button(button, false)
}

fn post_mouse_button(button: MouseButton, down: bool) -> anyhow::Result<()> {
    let (event_type, quartz_button) = match (button, down) {
        (MouseButton::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
        (MouseButton::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
        (MouseButton::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
        (MouseButton::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
        (MouseButton::Middle, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
        (MouseButton::Middle, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
    };
    let position = CGEvent::new(event_source()?)
        .map_err(|_| anyhow!("failed to query the Quartz cursor position"))?
        .location();
    let event = CGEvent::new_mouse_event(event_source()?, event_type, position, quartz_button)
        .map_err(|_| anyhow!("failed to create a Quartz mouse button event"))?;
    event.post(CGEventTapLocation::HID);
    Ok(())
}

pub(crate) async fn mouse_scroll(clicks: &f64, direction: &ScrollDirection) -> anyhow::Result<()> {
    let amount = clicks.round().max(1.0) as i32;
    let (vertical, horizontal) = match direction {
        ScrollDirection::Up => (amount, 0),
        ScrollDirection::Down => (-amount, 0),
        ScrollDirection::Left => (0, amount),
        ScrollDirection::Right => (0, -amount),
    };
    let event = CGEvent::new_scroll_event(
        event_source()?,
        ScrollEventUnit::LINE,
        2,
        vertical,
        horizontal,
        0,
    )
    .map_err(|_| anyhow!("failed to create a Quartz scroll event"))?;
    event.post(CGEventTapLocation::HID);
    Ok(())
}

fn display_scale() -> anyhow::Result<(f64, f64)> {
    let display = CGDisplay::main();
    let mode = display
        .display_mode()
        .ok_or_else(|| anyhow!("Quartz reported no mode for the main display"))?;
    Ok((
        mode.pixel_width() as f64 / mode.width() as f64,
        mode.pixel_height() as f64 / mode.height() as f64,
    ))
}

fn pixels_to_points(point: CGPoint) -> anyhow::Result<CGPoint> {
    let (scale_x, scale_y) = display_scale()?;
    Ok(scale_point(point, 1.0 / scale_x, 1.0 / scale_y))
}

fn points_to_pixels(point: CGPoint) -> anyhow::Result<CGPoint> {
    let (scale_x, scale_y) = display_scale()?;
    Ok(scale_point(point, scale_x, scale_y))
}

fn scale_point(point: CGPoint, scale_x: f64, scale_y: f64) -> CGPoint {
    CGPoint::new(point.x * scale_x, point.y * scale_y)
}

fn function_keycode(number: usize) -> anyhow::Result<CGKeyCode> {
    match number {
        1 => Ok(KeyCode::F1),
        2 => Ok(KeyCode::F2),
        3 => Ok(KeyCode::F3),
        4 => Ok(KeyCode::F4),
        5 => Ok(KeyCode::F5),
        6 => Ok(KeyCode::F6),
        7 => Ok(KeyCode::F7),
        8 => Ok(KeyCode::F8),
        9 => Ok(KeyCode::F9),
        10 => Ok(KeyCode::F10),
        11 => Ok(KeyCode::F11),
        12 => Ok(KeyCode::F12),
        _ => bail!("function key must be F1..F12"),
    }
}

fn keycode_for_literal(ch: char) -> anyhow::Result<CGKeyCode> {
    let keycode = match ch.to_ascii_lowercase() {
        'a' => KeyCode::ANSI_A,
        'b' => KeyCode::ANSI_B,
        'c' => KeyCode::ANSI_C,
        'd' => KeyCode::ANSI_D,
        'e' => KeyCode::ANSI_E,
        'f' => KeyCode::ANSI_F,
        'g' => KeyCode::ANSI_G,
        'h' => KeyCode::ANSI_H,
        'i' => KeyCode::ANSI_I,
        'j' => KeyCode::ANSI_J,
        'k' => KeyCode::ANSI_K,
        'l' => KeyCode::ANSI_L,
        'm' => KeyCode::ANSI_M,
        'n' => KeyCode::ANSI_N,
        'o' => KeyCode::ANSI_O,
        'p' => KeyCode::ANSI_P,
        'q' => KeyCode::ANSI_Q,
        'r' => KeyCode::ANSI_R,
        's' => KeyCode::ANSI_S,
        't' => KeyCode::ANSI_T,
        'u' => KeyCode::ANSI_U,
        'v' => KeyCode::ANSI_V,
        'w' => KeyCode::ANSI_W,
        'x' => KeyCode::ANSI_X,
        'y' => KeyCode::ANSI_Y,
        'z' => KeyCode::ANSI_Z,
        '0' => KeyCode::ANSI_0,
        '1' => KeyCode::ANSI_1,
        '2' => KeyCode::ANSI_2,
        '3' => KeyCode::ANSI_3,
        '4' => KeyCode::ANSI_4,
        '5' => KeyCode::ANSI_5,
        '6' => KeyCode::ANSI_6,
        '7' => KeyCode::ANSI_7,
        '8' => KeyCode::ANSI_8,
        '9' => KeyCode::ANSI_9,
        ' ' => KeyCode::SPACE,
        '-' => KeyCode::ANSI_MINUS,
        '=' => KeyCode::ANSI_EQUAL,
        '[' => KeyCode::ANSI_LEFT_BRACKET,
        ']' => KeyCode::ANSI_RIGHT_BRACKET,
        '\\' => KeyCode::ANSI_BACKSLASH,
        ';' => KeyCode::ANSI_SEMICOLON,
        '\'' => KeyCode::ANSI_QUOTE,
        ',' => KeyCode::ANSI_COMMA,
        '.' => KeyCode::ANSI_PERIOD,
        '/' => KeyCode::ANSI_SLASH,
        '`' => KeyCode::ANSI_GRAVE,
        _ => bail!("character {ch:?} has no Quartz chord keycode"),
    };
    Ok(keycode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_literal_keys_to_physical_quartz_keys() {
        assert_eq!(keycode_for_literal('a').unwrap(), KeyCode::ANSI_A);
        assert_eq!(keycode_for_literal('A').unwrap(), KeyCode::ANSI_A);
        assert_eq!(keycode_for_literal('/').unwrap(), KeyCode::ANSI_SLASH);
        assert!(keycode_for_literal('é').is_err());
    }

    #[test]
    fn maps_supported_function_keys() {
        assert_eq!(function_keycode(1).unwrap(), KeyCode::F1);
        assert_eq!(function_keycode(12).unwrap(), KeyCode::F12);
        assert!(function_keycode(13).is_err());
    }

    #[test]
    fn scales_each_coordinate_independently() {
        let scaled = scale_point(CGPoint::new(100.0, 75.0), 2.0, 1.5);
        assert_eq!(scaled.x, 200.0);
        assert_eq!(scaled.y, 112.5);
    }
}
