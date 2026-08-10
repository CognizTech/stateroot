# StateRoot one-line installer (Windows PowerShell).
#
#   irm https://github.com/CognizTech/stateroot/releases/latest/download/install.ps1 | iex
#
# Downloads the windows-x64 binary + checksums.txt from the latest release,
# verifies sha256 (fail closed), installs to %LOCALAPPDATA%\Programs\stateroot
# and adds it to the user PATH (setx-style via [Environment]).
#
# Pre-public testing:
#   $env:STATEROOT_INSTALL_BASE = "file:///C:/path/to/assets"; .\install.ps1
#Requires -Version 5.1
$ErrorActionPreference = 'Stop'

$Repo  = if ($env:STATEROOT_INSTALL_REPO) { $env:STATEROOT_INSTALL_REPO } else { 'CognizTech/stateroot' }
$Base  = if ($env:STATEROOT_INSTALL_BASE) { $env:STATEROOT_INSTALL_BASE } else { "https://github.com/$Repo/releases/latest/download" }
$Asset = 'stateroot-windows-x64.exe'

function Log($msg)  { Write-Host "stateroot-install: $msg" }
function Fail($msg) { Write-Host "stateroot-install: ERROR: $msg" -ForegroundColor Red; exit 1 }

$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
if ($arch -ne 'X64') { Fail "unsupported arch: $arch (need x64)" }

$Work = Join-Path $env:TEMP ("stateroot-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Work | Out-Null
try {
    function Fetch($url, $dest) {
        if ($url -like 'file://*') {
            $src = $url -replace '^file:///', '' -replace '/', '\'
            if (-not (Test-Path $src)) { Fail "missing local asset: $src" }
            Copy-Item $src $dest
        } else {
            try {
                Invoke-WebRequest -Uri $url -OutFile $dest -UseBasicParsing
            } catch {
                Fail "download failed: $url ($($_.Exception.Message))"
            }
        }
    }

    Log "fetching $Asset (+ checksums.txt) from $Base"
    Fetch "$Base/$Asset" (Join-Path $Work $Asset)
    Fetch "$Base/checksums.txt" (Join-Path $Work 'checksums.txt')

    # --- verify (fail closed) ---
    $lines = Get-Content (Join-Path $Work 'checksums.txt')
    $line = $lines | Where-Object { $_ -match "\s$Asset$" } | Select-Object -First 1
    if (-not $line) { Fail "checksums.txt has no entry for $Asset — refusing to install" }
    $expected = ($line -split '\s+')[0].ToLower()
    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $Work $Asset)).Hash.ToLower()
    if ($actual -ne $expected) { Fail "checksum mismatch for $Asset (expected $expected, got $actual)" }
    Log 'checksum verified'

    # --- install ---
    $DestDir = Join-Path $env:LOCALAPPDATA 'Programs\stateroot'
    New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
    $Dest = Join-Path $DestDir 'stateroot.exe'
    Copy-Item (Join-Path $Work $Asset) $Dest -Force
    Log "installed to $Dest"

    # --- PATH (user scope) ---
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not ($userPath -split ';' | Where-Object { $_ -eq $DestDir })) {
        [Environment]::SetEnvironmentVariable('Path', "$userPath;$DestDir", 'User')
        Log "added $DestDir to your user PATH — restart your shell for it to take effect"
    }

    Write-Host ''
    Write-Host 'Quickstart:'
    Write-Host '  1. cd your-project; stateroot init'
    Write-Host '  2. work in any harness (Claude, Codex, Cursor, Kimi, OpenClaw, Hermes)'
    Write-Host '  3. stateroot resume — anywhere, picks up the full working state'
} finally {
    Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue
}
