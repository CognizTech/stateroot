[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Msi,

    [Parameter(Mandatory = $true)]
    [string]$ExpectedVersion
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$msiPath = (Resolve-Path -LiteralPath $Msi).Path
$normalizedExpectedVersion = $ExpectedVersion.TrimStart('v')
$installDirectory = Join-Path $env:LOCALAPPDATA 'Programs\StateRoot'
$installedExecutable = Join-Path $installDirectory 'stateroot.exe'
$installLog = Join-Path $env:RUNNER_TEMP 'stateroot-msi-install.log'
$uninstallLog = Join-Path $env:RUNNER_TEMP 'stateroot-msi-uninstall.log'

function Write-MsiLog {
    param([string]$Path)

    if (Test-Path -LiteralPath $Path -PathType Leaf) {
        Write-Host "----- $Path (last 200 lines) -----"
        Get-Content -LiteralPath $Path -Tail 200 | Write-Host
    }
}

function Invoke-Msi {
    param(
        [ValidateSet('/i', '/x')]
        [string]$Action,
        [string]$LogPath
    )

    $quotedMsiPath = '"' + $msiPath + '"'
    $quotedLogPath = '"' + $LogPath + '"'
    $process = Start-Process -FilePath 'msiexec.exe' `
        -ArgumentList @($Action, $quotedMsiPath, '/qn', '/norestart', '/L*v', $quotedLogPath) `
        -Wait `
        -PassThru

    if ($process.ExitCode -notin @(0, 3010)) {
        Write-MsiLog -Path $LogPath
        throw "msiexec $Action failed with exit code $($process.ExitCode)."
    }
}

function ConvertTo-NormalizedPath {
    param([string]$Path)

    return [System.IO.Path]::GetFullPath($Path.Trim().Trim('"')).TrimEnd('\')
}

function Test-UserPathContains {
    param([string]$ExpectedPath)

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ([string]::IsNullOrWhiteSpace($userPath)) {
        return $false
    }

    $normalizedExpectedPath = ConvertTo-NormalizedPath -Path $ExpectedPath
    foreach ($entry in $userPath.Split(';', [StringSplitOptions]::RemoveEmptyEntries)) {
        try {
            $normalizedEntry = ConvertTo-NormalizedPath -Path $entry
        } catch {
            continue
        }

        if ($normalizedEntry.Equals($normalizedExpectedPath, [StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }

    return $false
}

$installationCompleted = $false
$primaryFailure = $null
$cleanupFailure = $null

try {
    Invoke-Msi -Action '/i' -LogPath $installLog
    $installationCompleted = $true

    if (-not (Test-Path -LiteralPath $installedExecutable -PathType Leaf)) {
        throw "The MSI completed without installing '$installedExecutable'."
    }

    $versionOutput = (& $installedExecutable --version 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) {
        throw "The installed CLI exited with code $LASTEXITCODE while checking its version."
    }

    $expectedOutput = "stateroot $normalizedExpectedVersion"
    if ($versionOutput -ne $expectedOutput) {
        throw "The installed CLI reported '$versionOutput', expected '$expectedOutput'."
    }

    if (-not (Test-UserPathContains -ExpectedPath $installDirectory)) {
        $persistedUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        throw "The MSI did not add '$installDirectory' to the user PATH. PATH='$persistedUserPath'"
    }
} catch {
    $primaryFailure = $_
} finally {
    if ($installationCompleted) {
        try {
            Invoke-Msi -Action '/x' -LogPath $uninstallLog
        } catch {
            $cleanupFailure = $_
        }
    }
}

if ($null -ne $primaryFailure) {
    Write-MsiLog -Path $installLog
    Write-MsiLog -Path $uninstallLog
    throw $primaryFailure
}

if ($null -ne $cleanupFailure) {
    Write-MsiLog -Path $uninstallLog
    throw $cleanupFailure
}

if (Test-Path -LiteralPath $installedExecutable) {
    throw "The MSI uninstall left '$installedExecutable' behind."
}

if (Test-UserPathContains -ExpectedPath $installDirectory) {
    throw "The MSI uninstall left '$installDirectory' in the user PATH."
}

Write-Host "Verified MSI install, version, user PATH, and uninstall for StateRoot $normalizedExpectedVersion."
