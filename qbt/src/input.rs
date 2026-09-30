use crate::pal;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
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
    PageUp,   // Note, called Page_Up in X11 keysym spelling
    Return,
    Right,
    Shift, // Note, called shift (lowercase) in X11 keysym spelling
    Super, // Note, called super (lowercase) in X11 keysym spelling
    Tab,
    Up,

    Typed(char),   // A typed character, e.g. 'a', '/', etc.
    Literal(char), // A character used in a chord, e.g. the 'a' in ctrl+a
}

#[derive(Default)]
struct HeldInputs {
    keys: Vec<Key>,
    mouse_buttons: Vec<pal::MouseButton>,
}

/// Owns every synthetic key and mouse button held by one agent connection.
/// The connection releases this complete set before its lifecycle ends.
#[derive(Default)]
pub(crate) struct SyntheticInput {
    held: Mutex<HeldInputs>,
    hold_tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl SyntheticInput {
    fn key_down(&self, key: Key) -> anyhow::Result<()> {
        pal::send_key_down(key)?;
        self.held.lock().unwrap().keys.push(key);
        Ok(())
    }

    fn key_up(&self, key: Key) -> anyhow::Result<()> {
        pal::send_key_up(key)?;
        let mut held = self.held.lock().unwrap();
        if let Some(index) = held.keys.iter().rposition(|candidate| *candidate == key) {
            held.keys.remove(index);
        }
        Ok(())
    }

    pub(crate) async fn mouse_down(&self, button: pal::MouseButton) -> anyhow::Result<()> {
        pal::mouse_down(button).await?;
        self.held.lock().unwrap().mouse_buttons.push(button);
        Ok(())
    }

    pub(crate) async fn mouse_up(&self, button: pal::MouseButton) -> anyhow::Result<()> {
        pal::mouse_up(button).await?;
        let mut held = self.held.lock().unwrap();
        if let Some(index) = held
            .mouse_buttons
            .iter()
            .rposition(|candidate| *candidate == button)
        {
            held.mouse_buttons.remove(index);
        }
        Ok(())
    }

    /// Releases all input still owned by this connection. Every release is
    /// attempted even if an earlier platform call fails.
    pub(crate) async fn release_all(&self) -> anyhow::Result<()> {
        for task in std::mem::take(&mut *self.hold_tasks.lock().unwrap()) {
            task.abort();
        }
        let (mut keys, mut mouse_buttons) = {
            let mut held = self.held.lock().unwrap();
            (
                std::mem::take(&mut held.keys),
                std::mem::take(&mut held.mouse_buttons),
            )
        };
        keys.reverse();
        mouse_buttons.reverse();
        let mut first_error = None;
        for key in keys {
            if let Err(error) = pal::send_key_up(key) {
                first_error.get_or_insert(error);
            }
        }
        for button in mouse_buttons {
            if let Err(error) = pal::mouse_up(button).await {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

pub(crate) async fn send_input_demo() -> anyhow::Result<()> {
    let mut keys = vec![Key::Super, Key::Literal('.')];
    eprintln!("keys pressing for {:?}", keys);
    for key in &keys {
        pal::send_key_down(*key)?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    keys.reverse();
    eprintln!("keys pressed");
    for key in keys.into_iter() {
        pal::send_key_up(key)?;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    eprintln!("keys up");

    let (start_x, start_y) = (10.0, 400.0);
    pal::mouse_move_to((start_x as i32, start_y as i32)).await?;

    Ok(())
}

// See libX11 X11/keysymdef.h for the key names.
//
/// Parses a keystroke expression into keys.
///
/// The syntax is what Anthropic models naturally emit for computer-use key
/// actions: '+'-separated tokens, each a whole key name or a single
/// character — `ctrl+alt+delete`, `meta+a`, `Return`, `escape`, `f5`.
/// Matching is case-insensitive and accepts the usual aliases (meta/cmd/win
/// for super, esc for escape, enter for return, del for delete). A token
/// that is neither a known key name nor a single character is an error, so
/// an unrecognized word can never silently type out its characters.
fn parse_keys(expression: &str) -> Result<Vec<Key>, String> {
    let mut keys = Vec::new();
    for token in expression.split('+') {
        keys.push(parse_key_token(token)?);
    }
    Ok(keys)
}

fn parse_key_token(token: &str) -> Result<Key, String> {
    if token.is_empty() {
        return Err("empty key: check for a doubled or trailing '+'".into());
    }
    let lower = token.to_ascii_lowercase();
    let key = match lower.as_str() {
        "alt" => Some(Key::Alt),
        "backspace" | "bs" => Some(Key::BackSpace),
        "ctrl" | "control" => Some(Key::Ctrl),
        "delete" | "del" => Some(Key::Delete),
        "down" => Some(Key::Down),
        "end" => Some(Key::End),
        "escape" | "esc" => Some(Key::Escape),
        "home" => Some(Key::Home),
        "left" => Some(Key::Left),
        "pagedown" | "page_down" => Some(Key::PageDown),
        "pageup" | "page_up" => Some(Key::PageUp),
        "return" | "enter" => Some(Key::Return),
        "right" => Some(Key::Right),
        "shift" => Some(Key::Shift),
        "space" => Some(Key::Typed(' ')),
        "plus" => Some(Key::Typed('+')),
        "super" | "meta" | "cmd" | "win" | "windows" => Some(Key::Super),
        "tab" => Some(Key::Tab),
        "up" => Some(Key::Up),
        _ => None,
    };
    if let Some(key) = key {
        return Ok(key);
    }
    if let Some(number) = lower.strip_prefix('f') {
        if let Ok(index) = number.parse::<usize>() {
            if (1..=12).contains(&index) {
                return Ok(Key::F(index));
            }
        }
    }
    let mut characters = token.chars();
    if let (Some(character), None) = (characters.next(), characters.next()) {
        return Ok(Key::Literal(character));
    }
    Err(format!(
        "unknown key '{token}': use a single character or one of alt, backspace, ctrl, delete, down, end, escape, f1..f12, home, left, pagedown, pageup, return, right, shift, space, super, tab, up (aliases: esc, del, enter, meta, cmd, win)"
    ))
}

pub(crate) async fn type_text(_input: &SyntheticInput, text: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    return pal::type_text(text).await;

    #[cfg(not(target_os = "linux"))]
    for ch in text.chars().into_iter() {
        let key = Key::Typed(ch);
        _input.key_down(key)?;
        tokio::time::sleep(Duration::from_millis(60)).await;
        _input.key_up(key)?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    #[cfg(not(target_os = "linux"))]
    Ok(())
}

async fn per_key<F: Fn(Key) -> anyhow::Result<()>>(keys: &str, fun: F) -> anyhow::Result<()> {
    let keys = parse_keys(keys).map_err(|error| {
        // The message tells the model how to fix the expression, not just
        // that it failed.
        eprintln!("error parsing keystroke '{keys}': {error}");
        anyhow::anyhow!("error parsing keystroke '{keys}': {error}")
    })?;
    for key in &keys {
        fun(*key)?;
        tokio::time::sleep(Duration::from_millis(6)).await;
    }
    Ok(())
}

pub(crate) async fn press_keys(input: &SyntheticInput, keys: &str) -> anyhow::Result<()> {
    per_key(keys, |key| input.key_down(key)).await?;
    Ok(())
}

pub(crate) async fn release_keys(input: &SyntheticInput, keys: &str) -> anyhow::Result<()> {
    per_key(keys, |key| input.key_up(key)).await?;
    Ok(())
}

// Presses the specified keys, then releases them. Returns after the keys have been be released.
pub(crate) async fn press_release_keys(input: &SyntheticInput, keys: &str) -> anyhow::Result<()> {
    let mut keys = parse_keys(keys)
        .map_err(|error| anyhow::anyhow!("error parsing keystroke '{keys}': {error}"))?;
    for key in &keys {
        input.key_down(*key)?;
        tokio::time::sleep(Duration::from_millis(6)).await;
    }
    tokio::time::sleep(Duration::from_millis(16)).await;
    keys.reverse();
    for key in keys {
        input.key_up(key)?;
        tokio::time::sleep(Duration::from_millis(4)).await;
    }
    Ok(())
}

// Presses the specified keys and returns. Asynchronously, after the specified duration has elapsed,
// releases the keys.
pub(crate) async fn hold_keys(
    input: Arc<SyntheticInput>,
    keys: &str,
    duration: Duration,
) -> anyhow::Result<()> {
    let keys = parse_keys(keys)
        .map_err(|error| anyhow::anyhow!("error parsing keystroke '{keys}': {error}"))?;
    for key in &keys {
        input.key_down(*key)?;
    }
    let task_input = input.clone();
    let task = tokio::task::spawn(async move {
        tokio::time::sleep(duration).await;
        eprintln!("releasing keys {:?}", keys);
        for key in keys {
            if let Err(error) = task_input.key_up(key) {
                eprintln!("failed to release held key: {error}");
            }
        }
    });
    let mut tasks = input.hold_tasks.lock().unwrap();
    tasks.retain(|task| !task.is_finished());
    tasks.push(task);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keystroke() {
        assert_eq!(parse_keys("a"), Ok(vec![Key::Literal('a')]));
    }

    #[test]
    fn parse_compound_keystroke() {
        assert_eq!(
            parse_keys("ctrl+space"),
            Ok(vec![Key::Ctrl, Key::Typed(' ')])
        );
    }

    #[test]
    fn parse_invalid_keystroke() {
        assert!(parse_keys("duper+A").is_err());
        // An unknown word must error, not silently type its characters.
        assert!(parse_keys("wondow").is_err());
        // A trailing or doubled separator is an error, not an empty key.
        assert!(parse_keys("ctrl+").is_err());
        assert!(parse_keys("ctrl++a").is_err());
        assert!(parse_keys("").is_err());
        assert!(parse_keys("+a").is_err());
        assert!(parse_keys("f0").is_err());
        assert!(parse_keys("f13").is_err());
    }

    #[test]
    fn parse_key_aliases_the_model_plausibly_emits() {
        assert_eq!(parse_keys("escape"), Ok(vec![Key::Escape]));
        assert_eq!(parse_keys("esc"), Ok(vec![Key::Escape]));
        assert_eq!(parse_keys("Enter"), Ok(vec![Key::Return]));
        assert_eq!(parse_keys("delete"), Ok(vec![Key::Delete]));
        assert_eq!(
            parse_keys("ctrl+alt+delete"),
            Ok(vec![Key::Ctrl, Key::Alt, Key::Delete])
        );
        assert_eq!(
            parse_keys("meta+a"),
            Ok(vec![Key::Super, Key::Literal('a')])
        );
        assert_eq!(parse_keys("cmd+c"), Ok(vec![Key::Super, Key::Literal('c')]));
        assert_eq!(parse_keys("F5"), Ok(vec![Key::F(5)]));
        assert_eq!(parse_keys("Return"), Ok(vec![Key::Return]));
        assert_eq!(parse_keys("windows"), Ok(vec![Key::Super]));
        assert_eq!(parse_keys("f1"), Ok(vec![Key::F(1)]));
        assert_eq!(parse_keys("f12"), Ok(vec![Key::F(12)]));
        assert_eq!(parse_keys("CtRl+A"), Ok(vec![Key::Ctrl, Key::Literal('A')]));
        assert_eq!(
            parse_keys("ctrl+plus"),
            Ok(vec![Key::Ctrl, Key::Typed('+')])
        );
        assert_eq!(parse_keys("f"), Ok(vec![Key::Literal('f')]));
    }

    #[tokio::test]
    async fn invalid_chord_sends_no_input() {
        let calls = std::cell::Cell::new(0);
        let result = per_key("ctrl+wondow", |_| {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .await;

        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unknown key 'wondow'")
        );
        assert_eq!(calls.get(), 0);
    }
}
