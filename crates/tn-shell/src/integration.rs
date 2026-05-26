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
}
