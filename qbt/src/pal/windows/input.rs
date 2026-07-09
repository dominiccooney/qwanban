use std::time::Duration;
use anyhow::anyhow;
use windows::Win32::UI::WindowsAndMessaging::{GetCursorInfo, CURSORINFO};
use winput::{Mouse, Input, Vk};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Key {
    Alt, // Note, called alt (lowercase) in X11 keysym spelling
    BackSpace,
    Ctrl, // Note, called ctrl (lowercase) in X11 keysym spelling
    Delete,
    Down,
    End,
    Escape,
    F(usize), // F1..F12
    Home,
    Left,
    PageDown, // Note, called Page_Down in X11 keysym spelling
    PageUp, // Note, called Page_Up in X11 keysym spelling
    Return,
    Right,
    Shift, // Note, called shift (lowercase) in X11 keysym spelling
    Super, // Note, called super (lowercase) in X11 keysym spelling
    Tab,
    Up,

    Typed(char), // A typed character, e.g. 'a', '/', etc.
}

pub(crate) fn key_for_character(ch: char) -> anyhow::Result<Key> {
    Ok(Key::Typed(ch))
}

fn input_of_key(key: Key, action: winput::Action) -> anyhow::Result<Input> {
    match key {
        Key::Typed(ch) => Input::from_char(ch, action).ok_or_else(|| anyhow!("invalid typed character '{}'", ch)),

        Key::Alt => Ok(Input::from_vk(Vk::Alt, action)),
        Key::BackSpace => Ok(Input::from_vk(Vk::Backspace, action)),
        Key::Ctrl => Ok(Input::from_vk(Vk::Control, action)),
        Key::Delete => Ok(Input::from_vk(Vk::Delete, action)),
        Key::Down => Ok(Input::from_vk(Vk::DownArrow, action)),
        Key::End => Ok(Input::from_vk(Vk::End, action)),
        Key::Escape => Ok(Input::from_vk(Vk::Escape, action)),
        Key::F(n) => Ok(Input::from_vk(match n {
            1 => Vk::F1,
            2 => Vk::F2,
            3 => Vk::F3,
            4 => Vk::F4,
            5 => Vk::F5,
            6 => Vk::F6,
            7 => Vk::F7,
            8 => Vk::F8,
            9 => Vk::F9,
            10 => Vk::F10,
            11 => Vk::F11,
            12 => Vk::F12,
            _ => unreachable!(),
        }, action)),
        Key::Home => Ok(Input::from_vk(Vk::Home, action)),
        Key::Left => Ok(Input::from_vk(Vk::LeftArrow, action)),
        Key::PageDown => Ok(Input::from_vk(Vk::PageDown, action)),
        Key::PageUp => Ok(Input::from_vk(Vk::PageUp, action)),
        Key::Return => Ok(Input::from_vk(Vk::Enter, action)),
        Key::Right => Ok(Input::from_vk(Vk::RightArrow, action)),
        Key::Shift => Ok(Input::from_vk(Vk::Shift, action)),
        Key::Super => Ok(Input::from_vk(Vk::LeftWin, action)),
        Key::Tab => Ok(Input::from_vk(Vk::Tab, action)),
        Key::Up => Ok(Input::from_vk(Vk::UpArrow, action)),
    }
}

pub(crate) fn send_key_down(key: Key) -> anyhow::Result<()> {
    let input = input_of_key(key, winput::Action::Press)?;
    if winput::send_inputs(&[input]) == 1 {
        Ok(())
    } else {
        Err(winput::WindowsError::from_last_error().into())
    }
}

pub(crate) fn send_key_up(key: Key) -> anyhow::Result<()> {
    let input = input_of_key(key, winput::Action::Release)?;
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
        let sigma = 1.0 / (1.0 + (-t).exp());
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