//! Windows shell helpers.

use anyhow::{Result, bail};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::PCWSTR;

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Opens a file, program or URL the way Explorer would. Unlike spawning a
/// process, this shows the administrator prompt for installers that need
/// it; `verb` "runas" asks for it even when the program does not.
pub fn shell_open(target: &OsStr, verb: &str) -> Result<()> {
    let target = wide(target);
    let verb = wide(OsStr::new(verb));
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // values up to 32 are error codes; 5 is also what a declined prompt gives
    let code = result.0 as isize;
    if code <= 32 {
        bail!("Windows could not open it (code {code})");
    }
    Ok(())
}
