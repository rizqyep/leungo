//! What the Start Menu shortcut runs on Windows: starts `leungo gui` next to
//! this program with no console window, and quits it when the dashboard
//! window is closed. `leungo.exe` stays a console program for the terminal.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::Command;

fn leungo() -> PathBuf {
    let name = if cfg!(windows) {
        "leungo.exe"
    } else {
        "leungo"
    };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

fn main() {
    let mut cmd = Command::new(leungo());
    cmd.args(["gui", "--quit-on-close"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    if cmd.spawn().is_err() {
        std::process::exit(1);
    }
}
