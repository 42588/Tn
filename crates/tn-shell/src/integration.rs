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

    /// Bash integration script (OSC 133 FTCS + 633 VS Code). Source it at
    /// session start. Uses `PROMPT_COMMAND` for D (exit code) + A (prompt
    /// start), appends B (command start) to PS1, and a DEBUG trap for
    /// E (command line) + C (output start).
    ///
    /// The script MUST be sourced via `--rcfile` (or equivalent) so that it
    /// runs *after* the user's own `.bashrc` (the rcfile sources `~/.bashrc`
    /// first, then adds hooks).
    pub fn bash(&self) -> String {
        const SCRIPT: &str = r#"
# Source the user's real bashrc first so aliases/prompt/env survive.
if [ -f ~/.bashrc ]; then
    . ~/.bashrc
fi

# Tn shell integration
__tn_nonce='__NONCE__'
__tn_in_cmd=0

# Escape a command line for OSC 633;E.
__tn_esc() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//;/\\x3b}"
    s="${s//$'\r'/\\x0d}"
    s="${s//$'\n'/\\x0a}"
    printf '%s' "$s"
}

# preexec: emit OSC 633;E (command line) + OSC 133;C (output start).
# The DEBUG trap fires for every simple command; we gate on __tn_in_cmd
# so only the first one per command-line emits E/C.
__tn_pe() {
    if [ "$__tn_in_cmd" -eq 1 ]; then return; fi
    local cmd="$BASH_COMMAND"
    [ -z "$cmd" ] && return
    case "$cmd" in __tn_*) return ;; esac
    __tn_in_cmd=1
    printf '\033]633;E;%s\007' "$(__tn_esc "$cmd")"
    printf '\033]133;C\007'
}
trap '__tn_pe' DEBUG

# precmd: emit OSC 133;D (exit code) + OSC 133;A (prompt start).
# MUST capture $? first - reading anything else resets it.
__tn_pc() {
    __tn_in_cmd=0
    local __tn_code=$?
    printf '\033]133;D;%s\007' "$__tn_code"
    printf '\033]133;A\007'
}

if [ -n "$PROMPT_COMMAND" ]; then
    PROMPT_COMMAND="__tn_pc;${PROMPT_COMMAND}"
else
    PROMPT_COMMAND="__tn_pc"
fi

# Append B (command start) marker after the prompt text.
PS1="${PS1}\[\033]133;B\007\]"
"#;
        SCRIPT.replace("__NONCE__", &self.nonce)
    }

    /// Zsh integration script (OSC 133 FTCS + 633 VS Code). Source it at
    /// session start. Uses `precmd` for D (exit code) + A (prompt start),
    /// `preexec` for E (command line) + C (output start), and appends B
    /// (command start) to PS1.
    ///
    /// The script sources the user's `.zshrc` first, then adds hooks - so
    /// aliases, prompt themes, and plugins are preserved.
    pub fn zsh(&self) -> String {
        const SCRIPT: &str = r#"
# Source the user's real zshrc first.
if [ -f ~/.zshrc ]; then
    . ~/.zshrc
fi

# Tn shell integration
__tn_nonce='__NONCE__'

# Escape a command line for OSC 633;E.
__tn_esc() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//;/\\x3b}"
    s="${s//$'\r'/\\x0d}"
    s="${s//$'\n'/\\x0a}"
    printf '%s' "$s"
}

# preexec: emit OSC 633;E (command line) + OSC 133;C (output start).
preexec() {
    printf '\033]633;E;%s\007' "$(__tn_esc "$1")"
    printf '\033]133;C\007'
}

# precmd: emit OSC 133;D (exit code) + OSC 133;A (prompt start).
precmd() {
    printf '\033]133;D;%s\007' "$?"
    printf '\033]133;A\007'
}

# Append B (command start) marker after the prompt text.
PS1="${PS1}%{\033]133;B\007%}"
"#;
        SCRIPT.replace("__NONCE__", &self.nonce)
    }

    /// PowerShell integration script (OSC 133 FTCS). Source it at session start.
    /// Wraps `prompt` to emit `D` (previous exit code) + `A` (prompt start) +
    /// `B` (command start); a PSReadLine Enter handler emits `C` (output start).
    ///
    /// NOTE: draft - to be verified against live pwsh in the M3 wiring phase
    /// (the `C` hook via PSReadLine especially needs on-machine confirmation).
    pub fn powershell(&self) -> String {
        const SCRIPT: &str = r#"
$global:__tn_nonce = '__NONCE__'
if (-not $global:__tn_orig_prompt) { $global:__tn_orig_prompt = $function:prompt }
function global:prompt {
  $ok = $?                      # capture FIRST - reading anything else resets $?
  $lec = $global:LASTEXITCODE
  # $LASTEXITCODE only tracks native exes; $? also covers cmdlet success, so a
  # succeeding cmdlet after a failed exe reports 0 (not the stale exit code).
  $code = if ($ok) { 0 } elseif ($lec) { $lec } else { 1 }
  $global:LASTEXITCODE = $lec   # restore for the wrapped prompt (oh-my-posh/starship)
  $e = [char]27
  $p = & $global:__tn_orig_prompt
  # OSC 633;P;Cwd — report the working directory each prompt so the file tree
  # follows `cd`. Only for a real FileSystem location (skip Cert:\ / HKLM:\ etc.,
  # which aren't browsable dirs and would re-root the explorer to a bogus path).
  $cwdseq = ''
  if ($PWD.Provider.Name -eq 'FileSystem') { $cwdseq = "$e]633;P;Cwd=$($PWD.ProviderPath)`a" }
  "$e]133;D;$code`a$cwdseq$e]133;A`a$p$e]133;B`a"
}
if (Get-Module -ListAvailable -Name PSReadLine) {
  Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
    $l = ''; $c = 0
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$l, [ref]$c)
    $e = [char]27
    # OSC 633;E carries the command line; escape ; \ CR LF (parser un-escapes \xHH).
    $cl = $l.Replace('\','\x5c').Replace(';','\x3b').Replace("`r",'\x0d').Replace("`n",'\x0a')
    [Console]::Write("$e]633;E;$cl`a$e]133;C`a")
    [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
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
        out.push(if chunk.len() > 1 {
            A[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_script_has_markers_and_nonce() {
        let i = Integration::new();
        assert!(!i.nonce.is_empty());
        let s = i.bash();
        for marker in ["]133;A", "]133;B", "]133;C", "]133;D", "]633;E"] {
            assert!(s.contains(marker), "bash script missing {marker}");
        }
        assert!(s.contains("$?"), "bash exit code must derive from $?");
        assert!(s.contains(&i.nonce));
        assert!(!s.contains("__NONCE__"));
        assert!(
            s.contains("PROMPT_COMMAND"),
            "bash script must use PROMPT_COMMAND"
        );
        assert!(s.contains("trap"), "bash script must use DEBUG trap");
        assert!(
            s.contains("BASH_COMMAND"),
            "bash script must read BASH_COMMAND"
        );
    }

    #[test]
    fn zsh_script_has_markers_and_nonce() {
        let i = Integration::new();
        assert!(!i.nonce.is_empty());
        let s = i.zsh();
        for marker in ["]133;A", "]133;B", "]133;C", "]133;D", "]633;E"] {
            assert!(s.contains(marker), "zsh script missing {marker}");
        }
        assert!(s.contains("$?"), "zsh exit code must derive from $?");
        assert!(s.contains(&i.nonce));
        assert!(!s.contains("__NONCE__"));
        assert!(s.contains("preexec()"), "zsh script must use preexec");
        assert!(s.contains("precmd()"), "zsh script must use precmd");
    }

    #[test]
    fn powershell_script_has_markers_and_nonce() {
        let i = Integration::new();
        assert!(!i.nonce.is_empty());
        let s = i.powershell();
        for marker in ["]133;A", "]133;B", "]133;C", "]133;D", "]633;E"] {
            assert!(s.contains(marker), "script missing {marker}");
        }
        assert!(
            s.contains("$?"),
            "exit code must derive from $? (not stale $LASTEXITCODE)"
        );
        assert!(s.contains(&i.nonce));
        assert!(!s.contains("__NONCE__"));
    }

    #[test]
    fn base64_known_vectors() {
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
        assert!(enc
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')));
        assert_eq!(enc.len() % 4, 0);
    }
}
