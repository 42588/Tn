//! WSL distro enumeration.
//!
//! A WSL session needs no special PTY backend — ConPTY hosts `wsl.exe` like any
//! other program (the driver layer spawns `wsl.exe -d <distro>` through
//! [`LocalPty`](crate::LocalPty)). This module just enumerates installed distros
//! so the launcher can list them. `wsl.exe --list --quiet` prints distro names
//! one per line in **UTF-16LE** (CR/LF terminated), which [`parse_distros`]
//! decodes; the parse is pure and unit-tested.

use std::process::Command;

/// Decode the UTF-16LE output of `wsl --list --quiet` into distro names.
/// Strips a leading BOM, blank lines, and stray NULs.
pub fn parse_distros(bytes: &[u8]) -> Vec<String> {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let text = String::from_utf16_lossy(&units);
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(|line| line.trim().trim_matches('\0').trim())
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Installed WSL distros (newest-registration order, as `wsl` reports them), or
/// an empty list if WSL isn't present / the call fails. Shells out to
/// `wsl.exe --list --quiet`; output is captured (no console window).
pub fn list_distros() -> Vec<String> {
    let mut cmd = Command::new("wsl.exe");
    cmd.args(["--list", "--quiet"]);
    // Don't flash a console window when called from the GUI process (release has
    // no console of its own).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.output() {
        Ok(out) if out.status.success() => parse_distros(&out.stdout),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `str` as UTF-16LE bytes, the way `wsl.exe` emits its list.
    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn parses_utf16le_distro_list() {
        let bytes = utf16le("docker-desktop\r\nDebian\r\nUbuntu\r\nAlmaLinux-9\r\n");
        assert_eq!(
            parse_distros(&bytes),
            vec!["docker-desktop", "Debian", "Ubuntu", "AlmaLinux-9"]
        );
    }

    #[test]
    fn strips_bom_and_blank_lines() {
        let bytes = utf16le("\u{feff}Ubuntu\r\n\r\nDebian\r\n");
        assert_eq!(parse_distros(&bytes), vec!["Ubuntu", "Debian"]);
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(parse_distros(&[]).is_empty());
        assert!(parse_distros(&utf16le("\r\n")).is_empty());
    }
}
