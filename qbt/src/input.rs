use std::time::Duration;
use chumsky::prelude::*;
use crate::pal;
use crate::pal::Key;

pub(crate) async fn send_input_demo() -> anyhow::Result<()> {
    let mut keys = vec![pal::Key::Super, Key::Literal('.')];
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

// See libX11 X11/keysymdef.h
// TODO: Complete this keymap
fn key_parser<'src>() -> impl Parser<'src, &'src str, Vec<Key>, extra::Err<Rich<'src, char>>> {
    let special_key = choice([
        just("BackSpace").to(Key::BackSpace),
        just("Delete").to(Key::Delete),
        just("Down").to(Key::Down),
        just("End").to(Key::End),
        just("Escape").to(Key::Escape),
        just("Home").to(Key::Home),
        just("Left").to(Key::Left),
        just("Page_Down").to(Key::PageDown),
        just("Page_Up").to(Key::PageUp),
        just("Return").to(Key::Return),
        just("Right").to(Key::Right),
        just("Tab").to(Key::Tab),
        just("Up").to(Key::Up),
        just("alt").to(Key::Alt),
        just("ctrl").to(Key::Ctrl),
        just("plus").to(Key::Typed('+')),
        just("shift").to(Key::Shift),
        just("space").to(Key::Typed(' ')),
        just("super").to(Key::Super),
    ]);
    let function_key = just('F')
        .ignore_then(text::digits(10).to_slice())
        .try_map(|s: &str, span| {
            s.parse::<usize>()
                .map_err(|e| Rich::custom(span, e.to_string()))
                .and_then(|num| {
                    if 1 <= num && num <= 12 {
                        Ok(Key::F(num))
                    } else {
                        Err(Rich::custom(span, "Function key must be F1..F12"))
                    }
                })
        });
    let literal_key = any().map(|ch| Key::Literal(ch));
    let key = choice([
        special_key.boxed(),
        function_key.boxed(),
        literal_key.boxed(),
    ]);
    let plus_delimiter = just('+');
    key.separated_by(plus_delimiter).at_least(1).collect::<Vec<_>>()
}

pub(crate) async fn type_text(text: &str) -> anyhow::Result<()> {
    for ch in text.chars().into_iter() {
        let key = pal::key_for_character(ch)?;
        pal::send_key_down(key)?;
        tokio::time::sleep(Duration::from_millis(60)).await;
        pal::send_key_up(key)?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    Ok(())
}

// Presses the specified keys, then releases them. Returns after the keys have been be released.
pub(crate) async fn press_release_keys(keys: &str) -> anyhow::Result<()> {
    let mut keys = match key_parser().parse(keys).into_result() {
        Err(es) => anyhow::bail!("{:?}", es),
        Ok(keys) => keys
    };
    for key in &keys {
        pal::send_key_down(*key)?;
        tokio::time::sleep(Duration::from_millis(6)).await;
    }
    tokio::time::sleep(Duration::from_millis(16)).await;
    keys.reverse();
    for key in keys {
        pal::send_key_up(key)?;
        tokio::time::sleep(Duration::from_millis(4)).await;
    }
    Ok(())
}

// Presses the specified keys and returns. Asynchronously, after the specified duration has elapsed,
// releases the keys.
pub(crate) async fn hold_keys(keys: &str, duration: Duration) -> anyhow::Result<()> {
    let keys = match key_parser().parse(keys).into_result() {
        Err(es) => anyhow::bail!("{:?}", es),
        Ok(keys) => keys
    };
    for key in &keys {
        pal::send_key_down(*key)?;
    }
    // TODO: return this future and track which keys are down when
    tokio::task::spawn(async move {
        tokio::time::sleep(duration).await;
        eprintln!("releasing keys {:?}", keys);
        for key in keys {
            pal::send_key_up(key).unwrap();
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use super::*;

    #[test]
    fn parse_keystroke() {
        assert_eq!(key_parser().parse("a").into_result(), Ok(vec![Key::Typed('a')]));
    }

    #[test]
    fn parse_compound_keystroke() {
        assert_eq!(key_parser().parse("ctrl+space").into_result(), Ok(vec![Key::Ctrl, Key::Typed(' ')]));
    }

    #[test]
    fn parse_invalid_keystroke() {
        assert_matches!(key_parser().parse("duper+A").into_result(), Err(_))
    }
}