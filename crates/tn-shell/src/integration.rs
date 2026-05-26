//! Per-session shell-integration scripts + an anti-spoof nonce.

/// Per-session integration state: a `nonce` (so a hostile program can't fake
/// command blocks by echoing OSC 133/633) plus the scripts that emit the markers
/// around the prompt and command.
pub struct Integration {
    /// Opaque per-session token; embedded in injected scripts and (later)
    /// validated on 633 sequences that carry it.
    pub nonce: String,
}

impl Default for Integration {
    fn default() -> Self {
        Self::new()
    }
}

impl Integration {
    /// Generate a per-session nonce without pulling in an RNG crate (process id
    /// + wall-clock nanos is enough to deter same-session spoofing).
    pub fn new() -> Self {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Self {
            nonce: format!("{pid:x}{nanos:x}"),
        }
    }

    /// PowerShell integration script (OSC 133 FTCS). Source it at session start.
    /// Wraps `prompt` to emit `D` (previous exit code) + `A` (prompt start) +
    /// `B` (command start); a PSReadLine Enter handler emits `C` (output start).
    ///
    /// NOTE: draft — to be verified against live pwsh in the M3 wiring phase
    /// (the `C` hook via PSReadLine especially needs on-machine confirmation).
    pub fn powershell(&self) -> String {
        const SCRIPT: &str = r#"
$global:__tn_nonce = '__NONCE__'
if (-not $global:__tn_orig_prompt) { $global:__tn_orig_prompt = $function:prompt }
function global:prompt {
  $code = $LASTEXITCODE; if ($null -eq $code) { $code = 0 }
  $e = [char]27
  $p = & $global:__tn_orig_prompt
  "$e]133;D;$code`a$e]133;A`a$p$e]133;B`a"
}
if (Get-Module -ListAvailable -Name PSReadLine) {
  Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
    [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
    [Console]::Write("$([char]27)]133;C`a")
  }
}
"#;
        SCRIPT.replace("__NONCE__", &self.nonce)
    }

    /// Base64 of the UTF-16LE bytes of [`Self::powershell`], for launching
    /// `powershell.exe -NoExit -EncodedCommand <b64>`: the FTCS hooks are
    /// sourced at startup with no temp file and no echoed input line.
    pub fn encoded_command(&self) -> String {
        let utf16: Vec<u8> = self
            .powershell()
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        base64(&utf16)
    }
}

/// Minimal standard-alphabet Base64 with `=` padding (avoids pulling a dep).
fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { A[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_script_has_markers_and_nonce() {
        let i = Integration::new();
        assert!(!i.nonce.is_empty());
        let s = i.powershell();
        for marker in ["]133;A", "]133;B", "]133;C", "]133;D"] {
            assert!(s.contains(marker), "script missing {marker}");
        }
        assert!(s.contains(&i.nonce));
        assert!(!s.contains("__NONCE__")); // placeholder substituted
    }

    #[test]
    fn base64_known_vectors() {
        // RFC 4648 §10 test vectors.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn encoded_command_is_base64_of_utf16le() {
        let i = Integration::new();
        let enc = i.encoded_command();
        assert!(!enc.is_empty());
        // valid base64 alphabet only
        assert!(enc
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')));
        // UTF-16LE of an ASCII-heavy script base64s to a length divisible by 4.
        assert_eq!(enc.len() % 4, 0);
    }
}
