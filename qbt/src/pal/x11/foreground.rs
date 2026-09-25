use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, GetPropertyReply};

use super::{ForegroundWindow, Observation, observe};
use crate::pal::x11_connection::{X11Connection, connection};

fn atom(x11: &X11Connection, name: &[u8]) -> Option<Atom> {
    let atom = x11.conn.intern_atom(true, name).ok()?.reply().ok()?.atom;
    (atom != 0).then_some(atom)
}

fn active_window(x11: &X11Connection, active: Atom) -> Option<u32> {
    let reply = x11
        .conn
        .get_property(false, x11.screen.root, active, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    parse_active_window(&reply)
}

fn parse_active_window(reply: &GetPropertyReply) -> Option<u32> {
    // EWMH defines one WINDOW, but xfwm4 appends a second value, so only the
    // requested first value is used and bytes_after is ignored.
    if reply.type_ != u32::from(AtomEnum::WINDOW) {
        return None;
    }
    let window = reply.value32()?.next()?;
    (window != 0).then_some(window)
}

fn title(x11: &X11Connection, window: u32) -> Option<String> {
    let utf8 = atom(x11, b"UTF8_STRING")?;
    let name = atom(x11, b"_NET_WM_NAME")?;
    let reply = x11
        .conn
        .get_property(false, window, name, utf8, 0, 8192)
        .ok()?
        .reply()
        .ok()?;
    parse_title(reply, utf8)
}

fn parse_title(reply: GetPropertyReply, utf8: Atom) -> Option<String> {
    // Reject unsupported encodings and partial values, preserving an empty
    // UTF8_STRING as a known untitled window rather than unavailable metadata.
    if reply.type_ != utf8 || reply.format != 8 || reply.bytes_after != 0 {
        return None;
    }
    String::from_utf8(reply.value).ok()
}

pub(super) fn sample() -> Option<Observation<u32>> {
    let x11 = connection().ok()?;
    let active = atom(x11, b"_NET_ACTIVE_WINDOW")?;
    observe(
        || active_window(x11, active),
        |window| ForegroundWindow {
            // _NET_WM_PID is supplied by the client and can identify a process on
            // another host. Neither it nor WM_CLASS proves a local executable.
            executable: None,
            title: title(x11, *window),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_window_uses_first_nonzero_window_id() {
        let reply = GetPropertyReply {
            type_: AtomEnum::WINDOW.into(),
            format: 32,
            value_len: 1,
            value: 42u32.to_ne_bytes().to_vec(),
            ..Default::default()
        };
        assert_eq!(parse_active_window(&reply), Some(42));
        // xfwm4 publishes `0x1c00003, 0x0`: a length-1 read leaves 4 bytes
        // after, and a longer read returns the trailing zero.
        for xfwm4 in [
            GetPropertyReply {
                bytes_after: 4,
                ..reply.clone()
            },
            GetPropertyReply {
                value_len: 2,
                value: [42u32, 0].iter().flat_map(|v| v.to_ne_bytes()).collect(),
                ..reply.clone()
            },
        ] {
            assert_eq!(parse_active_window(&xfwm4), Some(42));
        }
        for invalid in [
            GetPropertyReply {
                format: 8,
                ..reply.clone()
            },
            GetPropertyReply {
                type_: AtomEnum::CARDINAL.into(),
                ..reply.clone()
            },
            GetPropertyReply {
                value_len: 0,
                value: Vec::new(),
                ..reply.clone()
            },
            GetPropertyReply {
                value: 0u32.to_ne_bytes().to_vec(),
                ..reply
            },
        ] {
            assert_eq!(parse_active_window(&invalid), None);
        }
    }

    #[test]
    fn title_requires_complete_utf8_but_allows_empty() {
        let reply = GetPropertyReply {
            type_: 42,
            format: 8,
            ..Default::default()
        };
        assert_eq!(parse_title(reply.clone(), 42), Some(String::new()));
        assert_eq!(
            parse_title(
                GetPropertyReply {
                    value: "文書".as_bytes().to_vec(),
                    ..reply.clone()
                },
                42
            ),
            Some("文書".into())
        );
        for invalid in [
            GetPropertyReply {
                bytes_after: 4,
                ..reply.clone()
            },
            GetPropertyReply {
                format: 32,
                ..reply.clone()
            },
            GetPropertyReply {
                type_: 0,
                ..reply.clone()
            },
            GetPropertyReply {
                value: vec![255],
                ..reply
            },
        ] {
            assert_eq!(parse_title(invalid, 42), None);
        }
    }
}
