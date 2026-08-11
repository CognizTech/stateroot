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
$installDirectory = Join-Path $env:RUNNER_TEMP 'StateRoot Custom Install'
$installedExecutable = Join-Path $installDirectory 'stateroot.exe'
$installLog = Join-Path $env:RUNNER_TEMP 'stateroot-msi-install.log'
$settingsUninstallLog = Join-Path $env:RUNNER_TEMP 'stateroot-msi-settings-uninstall.log'
$fallbackUninstallLog = Join-Path $env:RUNNER_TEMP 'stateroot-msi-fallback-uninstall.log'
$installerRegistryKey = 'HKCU:\Software\CognizTech\StateRoot'

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
        [string]$LogPath,
        [string[]]$AdditionalArguments = @()
    )

    $quotedMsiPath = '"' + $msiPath + '"'
    $quotedLogPath = '"' + $LogPath + '"'
    $arguments = @($Action, $quotedMsiPath, '/qn', '/norestart', '/L*v', $quotedLogPath)
    $arguments += $AdditionalArguments
    $process = Start-Process -FilePath 'msiexec.exe' `
        -ArgumentList $arguments `
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
    $quotedInstallProperty = '"INSTALLFOLDER=' + $installDirectory + '"'
    Invoke-Msi -Action '/i' -LogPath $installLog -AdditionalArguments @($quotedInstallProperty)
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

    if (-not (Test-Path -LiteralPath $installerRegistryKey)) {
        throw "The MSI did not create its installer metadata registry key."
    }
    $installerMetadata = Get-ItemProperty -LiteralPath $installerRegistryKey
    if ((ConvertTo-NormalizedPath -Path $installerMetadata.InstallDir) -ne
        (ConvertTo-NormalizedPath -Path $installDirectory)) {
        throw "The MSI remembered '$($installerMetadata.InstallDir)', expected '$installDirectory'."
    }
    if ($installerMetadata.ProductCode -notmatch '^\{[0-9A-Fa-f-]{36}\}$') {
        throw "The MSI wrote an invalid ProductCode '$($installerMetadata.ProductCode)'."
    }

    # Windows Settings/msiexec removal must run StateRoot's cleanup-only path
    # before removing the executable. Use a known owned fixture to prove it.
    $cleanupFixture = Join-Path $env:USERPROFILE '.claude\commands\stateroot.md'
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $cleanupFixture) | Out-Null
    Set-Content -LiteralPath $cleanupFixture -Value 'StateRoot MSI cleanup fixture'
    Invoke-Msi -Action '/x' -LogPath $settingsUninstallLog
    $installationCompleted = $false
    if (Test-Path -LiteralPath $cleanupFixture) {
        throw "Windows Installer uninstall did not run StateRoot integration cleanup."
    }
    if (Test-Path -LiteralPath $installedExecutable) {
        throw "Windows Installer uninstall left '$installedExecutable' behind."
    }
    if (Test-Path -LiteralPath $installerRegistryKey) {
        throw "Windows Installer uninstall left installer metadata behind."
    }
    if (Test-UserPathContains -ExpectedPath $installDirectory) {
        throw "Windows Installer uninstall left '$installDirectory' in the user PATH."
    }

    # Reinstall, then prove `stateroot uninstall` delegates final removal back
    # to Windows Installer instead of self-deleting and stranding MSI state.
    Invoke-Msi -Action '/i' -LogPath $installLog -AdditionalArguments @($quotedInstallProperty)
    $installationCompleted = $true
    & $installedExecutable uninstall --yes
    if ($LASTEXITCODE -ne 0) {
        throw "The installed CLI exited with code $LASTEXITCODE during MSI-aware uninstall."
    }
    $deadline = [DateTime]::UtcNow.AddSeconds(60)
    while (((Test-Path -LiteralPath $installedExecutable) -or
            (Test-Path -LiteralPath $installerRegistryKey) -or
            (Test-UserPathContains -ExpectedPath $installDirectory)) -and
           [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 250
    }
    if (Test-Path -LiteralPath $installedExecutable) {
        throw "CLI-initiated MSI uninstall did not remove '$installedExecutable' within 60 seconds."
    }
    $installationCompleted = $false
} catch {
    $primaryFailure = $_
} finally {
    if ($installationCompleted) {
        try {
            Invoke-Msi -Action '/x' -LogPath $fallbackUninstallLog
        } catch {
            $cleanupFailure = $_
        }
    }
}

if ($null -ne $primaryFailure) {
    Write-MsiLog -Path $installLog
    Write-MsiLog -Path $settingsUninstallLog
    Write-MsiLog -Path $fallbackUninstallLog
    throw $primaryFailure
}

if ($null -ne $cleanupFailure) {
    Write-MsiLog -Path $fallbackUninstallLog
    throw $cleanupFailure
}

if (Test-Path -LiteralPath $installedExecutable) {
    throw "The MSI uninstall left '$installedExecutable' behind."
}

if (Test-UserPathContains -ExpectedPath $installDirectory) {
    throw "The MSI uninstall left '$installDirectory' in the user PATH."
}

if (Test-Path -LiteralPath $installerRegistryKey) {
    throw "The final uninstall left installer metadata behind."
}

Write-Host "Verified custom-path MSI install, metadata, Windows Settings cleanup, CLI uninstall delegation, PATH, and version for StateRoot $normalizedExpectedVersion."
