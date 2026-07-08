use std::time::Duration;
use windows::Win32::UI::WindowsAndMessaging::{GetCursorInfo, CURSORINFO};
use winput::{Mouse, Input};

pub(crate) type Key = char;

pub(crate) fn key_for_character(ch: char) -> anyhow::Result<Key> {
    Ok(ch)
}

pub(crate) fn send_key_down(ch: Key) -> anyhow::Result<()> {
    let input = match Input::from_char(ch, winput::Action::Press) {
        None => anyhow::bail!("could not find key for character '{}'", ch),
        Some(input) => input,
    };
    if winput::send_inputs(&[input]) == 1 {
        Ok(())
    } else {
        Err(winput::WindowsError::from_last_error().into())
    }
}

pub(crate) fn send_key_up(ch: Key) -> anyhow::Result<()> {
    let input = match Input::from_char(ch, winput::Action::Release) {
        None => anyhow::bail!("could not find key for character '{}'", ch),
        Some(input) => input,
    };
    if winput::send_inputs(&[input]) == 1 {
        Ok(())
    } else {
        Err(winput::WindowsError::from_last_error().into())
    }
}

pub(crate) fn cursor_position() -> anyhow::Result<(usize, usize)> {
    let mut cursor_info = CURSORINFO {
        cbSize: size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetCursorInfo(&mut cursor_info)?; }
    Ok((cursor_info.ptScreenPos.x as usize, cursor_info.ptScreenPos.y as usize))
}

pub(crate) async fn mouse_move_to((end_x, end_y): (i32, i32)) -> anyhow::Result<()> {
    let (start_x, start_y) = cursor_position()?;
    let start_x = start_x as i32;
    let start_y = start_y as i32;

    let distance = (((start_x - end_x).pow(2) + (start_y - end_y).pow(2)) as f64).sqrt();
    let steps = (distance / 20.0).ceil() as usize;

    for i in 0..steps {
        let t: f64 = -6.0 + 12.0 * i as f64 / steps as f64;
        let sigma = 1.0 / (1.0 + -t.exp());
        let x = start_x + ((end_x - start_x) as f64 * sigma) as i32;
        let y = start_y + ((end_y - start_y) as f64 * sigma) as i32;
        Mouse::set_position(x, y)?;
        tokio::time::sleep(Duration::from_millis(4)).await;
    }

    Mouse::set_position(end_x, end_y)?;
    Ok(())
}

#[derive(Copy, Clone)]
pub(crate) enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    fn to_winput(self) -> winput::Button {
        match self {
            MouseButton::Left => winput::Button::Left,
            MouseButton::Middle => winput::Button::Middle,
            MouseButton::Right => winput::Button::Right,
        }
    }
}

// TODO: Modifier clicks.
pub(crate) async fn mouse_down(button: MouseButton) -> anyhow::Result<()> {
    let input = Input::from_button(button.to_winput(), winput::Action::Press);
    if winput::send_inputs(&[input]) == 1 {
        Ok(())
    } else {
        Err(winput::WindowsError::from_last_error().into())
    }
}

pub(crate) async fn mouse_up(button: MouseButton) -> anyhow::Result<()> {
    let input = Input::from_button(button.to_winput(), winput::Action::Release);
    if winput::send_inputs(&[input]) == 1 {
        Ok(())
    } else {
        Err(winput::WindowsError::from_last_error().into())
    }
}