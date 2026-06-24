use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Translate a key press into the bytes a PTY expects. Returns an empty vec
/// for keys we don't forward. `Ctrl-q` is intercepted by the caller (focus
/// toggle) before this is called, so it is not special-cased here.
pub fn encode_key(key: KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) if ctrl && c.is_ascii_alphabetic() => {
            vec![(c.to_ascii_lowercase() as u8) & 0x1f]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        _ => Vec::new(),
    }
}

/// Encode a mouse-wheel tick as an SGR mouse report (`ESC [ < Cb ; Cx ; Cy M`)
/// for the PTY. The right pane is an embedded tmux client with `mouse on`, so
/// forwarding the wheel lets tmux scroll its own scrollback (copy-mode) — i.e.
/// show the old logs — or hand the wheel to a full-screen app that grabbed the
/// mouse. Wheel-up is button 64, wheel-down 65; `col`/`row` are 1-based and
/// relative to the terminal pane's top-left.
pub fn encode_wheel(up: bool, col: u16, row: u16) -> Vec<u8> {
    let button = if up { 64 } else { 65 };
    format!("\x1b[<{button};{col};{row}M").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn printable_chars_are_utf8() {
        assert_eq!(encode_key(k(KeyCode::Char('a'))), b"a");
        assert_eq!(encode_key(k(KeyCode::Char('Z'))), b"Z");
        assert_eq!(encode_key(k(KeyCode::Char('é'))), "é".as_bytes());
    }

    #[test]
    fn control_chars_map_to_control_bytes() {
        assert_eq!(encode_key(ctrl('c')), vec![0x03]);
        assert_eq!(encode_key(ctrl('a')), vec![0x01]);
        assert_eq!(encode_key(ctrl('z')), vec![0x1a]);
    }

    #[test]
    fn special_keys_map_to_sequences() {
        assert_eq!(encode_key(k(KeyCode::Enter)), vec![b'\r']);
        assert_eq!(encode_key(k(KeyCode::Tab)), vec![b'\t']);
        assert_eq!(encode_key(k(KeyCode::Backspace)), vec![0x7f]);
        assert_eq!(encode_key(k(KeyCode::Esc)), vec![0x1b]);
        assert_eq!(encode_key(k(KeyCode::Up)), b"\x1b[A".to_vec());
        assert_eq!(encode_key(k(KeyCode::Down)), b"\x1b[B".to_vec());
        assert_eq!(encode_key(k(KeyCode::Right)), b"\x1b[C".to_vec());
        assert_eq!(encode_key(k(KeyCode::Left)), b"\x1b[D".to_vec());
        assert_eq!(encode_key(k(KeyCode::Home)), b"\x1b[H".to_vec());
        assert_eq!(encode_key(k(KeyCode::End)), b"\x1b[F".to_vec());
        assert_eq!(encode_key(k(KeyCode::Delete)), b"\x1b[3~".to_vec());
        assert_eq!(encode_key(k(KeyCode::PageUp)), b"\x1b[5~".to_vec());
        assert_eq!(encode_key(k(KeyCode::PageDown)), b"\x1b[6~".to_vec());
    }

    #[test]
    fn unmapped_keys_produce_no_bytes() {
        assert!(encode_key(k(KeyCode::F(5))).is_empty());
        assert!(encode_key(k(KeyCode::Insert)).is_empty());
    }

    #[test]
    fn wheel_encodes_sgr_with_button_and_coords() {
        // Wheel-up at column 3, row 7 (1-based, pane-relative).
        assert_eq!(encode_wheel(true, 3, 7), b"\x1b[<64;3;7M".to_vec());
        // Wheel-down uses button 65.
        assert_eq!(encode_wheel(false, 12, 1), b"\x1b[<65;12;1M".to_vec());
    }
}
