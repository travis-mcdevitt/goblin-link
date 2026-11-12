//! Scripted input for on-device and headless tests: when `GOBLIN_UI_INPUT_FIFO` names a FIFO,
//! each line written to it is one button press (`a`, `b`, `down`, `select`, …) or `quit`.
//!
//! This lets a test drive the app without injecting events into the shared input devices,
//! which would also reach the paused launcher and replay there when it resumes. Off unless the
//! variable is set; the FIFO is read without blocking.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::os::unix::fs::OpenOptionsExt;

use goblin_ui_kit::Button;

/// One scripted command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scripted {
    /// Press and release a button.
    Tap(Button),
    /// Leave the app.
    Quit,
}

/// Reads commands from the FIFO named by `GOBLIN_UI_INPUT_FIFO`.
pub struct ScriptInput {
    file: File,
    buf: Vec<u8>,
}

impl ScriptInput {
    /// Opens the FIFO if the variable is set. Errors are reported and disable the hook.
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os("GOBLIN_UI_INPUT_FIFO")?;
        match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => {
                eprintln!("goblin-ui: scripted input from {}", path.to_string_lossy());
                Some(ScriptInput {
                    file,
                    buf: Vec::new(),
                })
            }
            Err(e) => {
                eprintln!("goblin-ui: scripted input disabled: {e}");
                None
            }
        }
    }

    /// Returns the complete commands received since the last call.
    pub fn poll(&mut self) -> Vec<Scripted> {
        let mut chunk = [0u8; 256];
        loop {
            match self.file.read(&mut chunk) {
                Ok(0) => break, // no writer right now
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line);
            match parse(line.trim()) {
                Some(c) => out.push(c),
                None if line.trim().is_empty() => {}
                None => eprintln!("goblin-ui: unknown scripted input {:?}", line.trim()),
            }
        }
        out
    }
}

/// Parses one command line.
pub fn parse(s: &str) -> Option<Scripted> {
    let b = match s.to_ascii_lowercase().as_str() {
        "quit" => return Some(Scripted::Quit),
        "up" => Button::Up,
        "down" => Button::Down,
        "left" => Button::Left,
        "right" => Button::Right,
        "a" => Button::A,
        "b" => Button::B,
        "x" => Button::X,
        "y" => Button::Y,
        "l1" => Button::L1,
        "r1" => Button::R1,
        "l2" => Button::L2,
        "r2" => Button::R2,
        "select" => Button::Select,
        "start" => Button::Start,
        "menu" => Button::Menu,
        "f1" => Button::F1,
        "f2" => Button::F2,
        _ => return None,
    };
    Some(Scripted::Tap(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_buttons_and_quit() {
        assert_eq!(parse("A"), Some(Scripted::Tap(Button::A)));
        assert_eq!(parse("select"), Some(Scripted::Tap(Button::Select)));
        assert_eq!(parse("quit"), Some(Scripted::Quit));
        assert_eq!(parse("jump"), None);
    }
}
