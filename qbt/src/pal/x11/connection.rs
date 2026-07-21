use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use anyhow::{anyhow, Context};
use x11rb::connection::Connection as _;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, Screen};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use xkeysym::Keysym;

/// The keyboard state needed to resolve a keysym (e.g. "a", "Escape") to a keycode that
/// XTEST FakeInput can use: the keyboard's current keysym-per-keycode mapping, and a spare
/// keycode we remap on demand to type Unicode characters outside the current keyboard
/// layout, the same technique xdotool and enigo use.
struct KeyboardState {
    keysyms_per_keycode: u8,
    keysym_to_keycode: HashMap<Keysym, u8>,
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

        // Only trust the keyboard's own mapping for a keysym that sits at shift level 0
        // (column 0, no modifier held). Keysyms at other levels (e.g. the digit row's
        // shifted symbols) would otherwise get typed as their unshifted equivalent, since
        // FakeInput presses just the keycode without also holding Shift.
        let mut keysym_to_keycode = HashMap::new();
        let mut unused_keycode = None;
        for (row, chunk) in reply.keysyms.chunks(keysyms_per_keycode).enumerate() {
            let keycode = min_keycode + row as u8;
            if chunk.iter().all(|&raw| raw == 0) {
                unused_keycode = Some(keycode);
            }
            if let Some(&level0) = chunk.first() {
                if level0 != 0 {
                    keysym_to_keycode.entry(Keysym::from(level0)).or_insert(keycode);
                }
            }
        }

        Ok(Self {
            keysyms_per_keycode: reply.keysyms_per_keycode,
            keysym_to_keycode,
            // Prefer a keycode with no assigned keysyms; fall back to the highest legal
            // keycode, which is conventionally left unused for exactly this purpose.
            scratch_keycode: unused_keycode.unwrap_or(max_keycode),
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
