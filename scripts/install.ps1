# Tn install script.
#   Pre-built zip:  right-click install.ps1 -> Run with PowerShell
#   Source repo:    powershell -File scripts\install.ps1
$ErrorActionPreference = "Stop"

$InstallDir = "$env:LOCALAPPDATA\Programs\Tn"
$Shortcut   = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Tn.lnk"
$UninstKey  = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Tn"

# Find tn.exe: 1) next to script (pre-built zip)  2) target/release  3) cargo build
$exe = $null
if ($PSScriptRoot) {
    if (Test-Path "$PSScriptRoot\tn.exe") { $exe = "$PSScriptRoot\tn.exe" }
    if (-not $exe -and (Test-Path "$PSScriptRoot\..\target\release\tn.exe")) {
        $exe = "$PSScriptRoot\..\target\release\tn.exe"
    }
}
if (-not $exe -and (Test-Path "target\release\tn.exe")) {
    $exe = (Resolve-Path "target\release\tn.exe").Path
}
if (-not $exe) {
    Write-Host "  cargo build --release (first time, may take a few minutes) ..." -ForegroundColor Yellow
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "build failed" }
    $exe = (Resolve-Path "target\release\tn.exe").Path
}
if (-not $exe -or -not (Test-Path $exe)) { throw "tn.exe not found" }

Write-Host "Tn install ..." -ForegroundColor Cyan

# Copy
Write-Host "  install to $InstallDir ..." -NoNewline
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item -Force $exe $InstallDir
Write-Host " done"

# PATH
$p = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($p -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("PATH", "$p;$InstallDir", "User")
    $env:PATH = "$env:PATH;$InstallDir"
}

# Shortcut
$lnk = (New-Object -ComObject WScript.Shell).CreateShortcut($Shortcut)
$lnk.TargetPath = "$InstallDir\tn.exe"
$lnk.WorkingDirectory = [Environment]::GetFolderPath("UserProfile")
$lnk.IconLocation = "$InstallDir\tn.exe"
$lnk.Save()

# Uninstall registry
New-Item -Force -Path $UninstKey | Out-Null
@{
    DisplayName     = "Tn Terminal"
    DisplayIcon     = "$InstallDir\tn.exe"
    UninstallString = "powershell -Command `"Remove-Item -Recurse -Force '$Shortcut','$InstallDir'; Remove-Item -Force '$UninstKey'`""
    NoModify        = 1
    NoRepair        = 1
}.GetEnumerator() | ForEach-Object { Set-ItemProperty -Path $UninstKey -Name $_.Key -Value $_.Value }

Write-Host "Done! Open Win+R, type tn, press Enter." -ForegroundColor Green
