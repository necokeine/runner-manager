//! Copy text to the system clipboard.
//!
//! `runner-manager` runs on the alternate screen *outside* tmux and owns its
//! own stdout, so it can talk to the host terminal directly. Selecting text in
//! the embedded session (`select.rs`) routes here. Two paths are used together,
//! belt-and-braces, because no single one works everywhere:
//!
//! - **OSC 52** written to stdout — the portable, SSH-friendly path most modern
//!   terminals (iTerm2, kitty, wezterm, …) honour without any extra process.
//! - a **local clipboard tool** (`pbcopy` on macOS, `wl-copy`/`xclip`/`xsel` on
//!   Linux) — covers terminals that ignore OSC 52, e.g. macOS Terminal.app.
//!
//! Both are best-effort: a failure on either path is silently ignored since the
//! other usually covers it.

use std::io::{self, Write};
use std::process::{Command, Stdio};

/// Standard base64 alphabet (RFC 4648), used to encode the OSC 52 payload.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode bytes as standard, padded base64. Tiny and dependency-free: the only
/// caller is the OSC 52 payload, which is short (a clipboard selection).
pub fn base64(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// The OSC 52 escape that sets the system clipboard (`c`) to `text`.
pub fn osc52(text: &str) -> Vec<u8> {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes())).into_bytes()
}

/// Copy `text` to the system clipboard via OSC 52 and, best-effort, a local
/// clipboard tool. No-op for empty text.
pub fn copy(text: &str) {
    if text.is_empty() {
        return;
    }
    // OSC 52 reaches the host terminal directly (we are not inside tmux).
    let mut out = io::stdout();
    let _ = out.write_all(&osc52(text));
    let _ = out.flush();
    // Local tool fallback for terminals that drop OSC 52.
    system_copy(text);
}

/// Pipe `text` into the first available platform clipboard command. Purely
/// best-effort: a missing tool or a failed pipe is silently skipped.
fn system_copy(text: &str) {
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["-ib"]),
        ]
    };
    for (cmd, args) in candidates {
        let spawned = Command::new(cmd)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(_) => continue, // tool not installed — try the next one
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
            // stdin drops here, closing the pipe so the tool can finish.
        }
        let _ = child.wait();
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 examples, including the two padding cases.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_wraps_the_base64_payload() {
        assert_eq!(osc52("foo"), b"\x1b]52;c;Zm9v\x07".to_vec());
    }
}
