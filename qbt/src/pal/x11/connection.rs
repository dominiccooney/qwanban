use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use anyhow::{anyhow, Context};
use x11rb::connection::Connection as _;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, Screen};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use xkeysym::Keysym;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct KeyStroke {
    pub(crate) keycode: u8,
    pub(crate) shift: bool,
}

/// The keyboard state needed to resolve a keysym (e.g. "a", "Escape") to a keycode that
/// XTEST FakeInput can use: the keyboard's current keysym-per-keycode mapping, and a spare
/// keycode we remap on demand to type Unicode characters outside the current keyboard
/// layout, the same technique xdotool and enigo use.
struct KeyboardState {
    keysyms_per_keycode: u8,
    keysym_to_keycode: HashMap<Keysym, u8>,
    typed_keysym_to_keystroke: HashMap<Keysym, KeyStroke>,
    unused_typed_keycodes: VecDeque<u8>,
    scratch_keycode: u8,
    scratch_mapped_keysym: Option<Keysym>,
}

impl KeyboardState {
    fn load(conn: &RustConnection) -> anyhow::Result<Self> {
        let setup = conn.setup();
        let (min_keycode, max_keycode) = (setup.min_keycode, setup.max_keycode);
        // X11 guarantees min_keycode >= 8, so this always fits in a u8, but compute it via
        // u16 to avoid ever panicking on subtraction overflow in a malformed setup.
        let count = (max_keycode as u16 - min_keycode as u16 + 1) as u8;

        let reply = conn.get_keyboard_mapping(min_keycode, count)?.reply()
            .context("querying the keyboard mapping")?;
        let keysyms_per_keycode = reply.keysyms_per_keycode as usize;

        let mut keysym_to_keycode = HashMap::new();
        let mut typed_keysym_to_keystroke = HashMap::new();
        let mut unused_keycodes = VecDeque::new();
        for (row, chunk) in reply.keysyms.chunks(keysyms_per_keycode).enumerate() {
            let keycode = min_keycode + row as u8;
            if chunk.iter().all(|&raw| raw == 0) {
                unused_keycodes.push_back(keycode);
            }
            if let Some(&level0) = chunk.first()
                && level0 != 0
            {
                keysym_to_keycode.entry(Keysym::from(level0)).or_insert(keycode);
            }
            for (level, &raw) in chunk.iter().take(2).enumerate() {
                if raw != 0 {
                    typed_keysym_to_keystroke
                        .entry(Keysym::from(raw))
                        .or_insert(KeyStroke {
                            keycode,
                            shift: level == 1,
                        });
                }
            }
        }

        let scratch_keycode = unused_keycodes.pop_back().unwrap_or(max_keycode);
        Ok(Self {
            keysyms_per_keycode: reply.keysyms_per_keycode,
            keysym_to_keycode,
            typed_keysym_to_keystroke,
            unused_typed_keycodes: unused_keycodes,
            // Keep the chord scratch keycode separate from stable text mappings so neither
            // path can invalidate the other's cached keycode meaning.
            scratch_keycode,
            scratch_mapped_keysym: None,
        })
    }
}

/// A shared connection to the X server, used by both screen capture and input simulation.
pub(crate) struct X11Connection {
    pub(crate) conn: RustConnection,
    pub(crate) screen: Screen,
    keyboard: Mutex<Option<KeyboardState>>,
}

fn connect() -> anyhow::Result<X11Connection> {
    let (conn, screen_num) = RustConnection::connect(None).context("connecting to the X11 server")?;
    let screen = conn.setup().roots[screen_num].clone();

    // XFixes requires the client to negotiate a version before using its requests, such as
    // GetCursorImage, which we use to composite the cursor into screenshots.
    conn.xfixes_query_version(6, 0)?.reply().context("negotiating the XFIXES extension version")?;
    // XTEST doesn't strictly require this, but negotiating a version up front surfaces a
    // clear error immediately if the extension is missing, rather than on the first input.
    conn.xtest_get_version(2, 2)?.reply().context("negotiating the XTEST extension version")?;

    Ok(X11Connection {
        conn,
        screen,
        keyboard: Mutex::new(None),
    })
}

/// Returns the shared connection to the X server, establishing it on first use.
pub(crate) fn connection() -> anyhow::Result<&'static X11Connection> {
    static CONNECTION: OnceLock<Result<X11Connection, String>> = OnceLock::new();
    CONNECTION
        .get_or_init(|| connect().map_err(|err| format!("{err:#}")))
        .as_ref()
        .map_err(|err| anyhow!("{err}"))
}

/// Resolves an entire text action to stable keycodes before any key event is sent. X11 key
/// events carry only a keycode, so these mappings remain installed for the process lifetime;
/// a receiving application can then translate delayed events without observing a later
/// character's mapping. Every mapping level contains the same symbol so Shift and Caps Lock
/// cannot change literal typed text.
pub(crate) fn keystrokes_for_text(x11: &X11Connection, text: &str) -> anyhow::Result<Vec<KeyStroke>> {
    let mut guard = x11.keyboard.lock().unwrap();
    if guard.is_none() {
        *guard = Some(KeyboardState::load(&x11.conn)?);
    }
    let state = guard.as_mut().unwrap();

    let mut missing_keysyms = Vec::new();
    for keysym in text.chars().map(Keysym::from_char) {
        if !state.typed_keysym_to_keystroke.contains_key(&keysym)
            && !missing_keysyms.contains(&keysym)
        {
            missing_keysyms.push(keysym);
        }
    }
    if missing_keysyms.len() > state.unused_typed_keycodes.len() {
        anyhow::bail!(
            "typing requires {} new X11 key mappings, but only {} unused keycodes remain",
            missing_keysyms.len(),
            state.unused_typed_keycodes.len()
        );
    }

    for keysym in missing_keysyms {
        let keycode = state.unused_typed_keycodes.pop_front().unwrap();
        let row = stable_mapping_row(keysym, state.keysyms_per_keycode);
        x11.conn.change_keyboard_mapping(
            1,
            keycode,
            state.keysyms_per_keycode,
            &row,
        )?.check().context("installing a stable typed-character mapping")?;
        state.typed_keysym_to_keystroke.insert(
            keysym,
            KeyStroke {
                keycode,
                shift: false,
            },
        );
    }
    x11.conn.sync().context("synchronizing typed-character mappings")?;

    Ok(text
        .chars()
        .map(Keysym::from_char)
        .map(|keysym| state.typed_keysym_to_keystroke[&keysym])
        .collect())
}

fn stable_mapping_row(keysym: Keysym, keysyms_per_keycode: u8) -> Vec<u32> {
    vec![keysym.raw(); keysyms_per_keycode as usize]
}

/// Resolves a keyboard symbol to a keycode that XTEST FakeInput can use, remapping the
/// connection's scratch keycode if the symbol isn't reachable through the current keyboard
/// layout (for example, an arbitrary typed Unicode character).
pub(crate) fn keycode_for_keysym(x11: &X11Connection, keysym: Keysym) -> anyhow::Result<u8> {
    let mut guard = x11.keyboard.lock().unwrap();
    if guard.is_none() {
        *guard = Some(KeyboardState::load(&x11.conn)?);
    }
    let state = guard.as_mut().unwrap();

    if let Some(&keycode) = state.keysym_to_keycode.get(&keysym) {
        return Ok(keycode);
    }

    if state.scratch_mapped_keysym != Some(keysym) {
        let mut row = vec![0u32; state.keysyms_per_keycode as usize];
        row[0] = keysym.raw();
        x11.conn.change_keyboard_mapping(1, state.scratch_keycode, state.keysyms_per_keycode, &row)?
            .ignore_error();
        x11.conn.flush()?;
        state.scratch_mapped_keysym = Some(keysym);
    }
    Ok(state.scratch_keycode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_typed_mapping_uses_the_same_symbol_at_every_level() {
        assert_eq!(
            stable_mapping_row(Keysym::from_char('_'), 4),
            vec!['_' as u32; 4]
        );
        assert_eq!(
            stable_mapping_row(Keysym::from_char('A'), 2),
            vec!['A' as u32; 2]
        );
    }
}
