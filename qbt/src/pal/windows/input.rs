use winput::{Vk, Input};

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