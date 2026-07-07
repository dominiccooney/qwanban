use std::time::Duration;
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

pub(crate) async fn mouse_move_to((x, y): (i32, i32)) -> anyhow::Result<()> {
    // TODO: Add acceleration and jitter so bot detection isn't triggered by this.
    // TODO: Synthesize actual inputs and return errors.
    Mouse::set_position(x, y)?;
    tokio::time::sleep(Duration::from_millis(8)).await;
    Ok(())
}

pub(crate) enum MouseButton {
    Left,
    Right,
}

impl MouseButton {
    fn to_winput(self) -> winput::Button {
        match self {
            MouseButton::Left => winput::Button::Left,
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