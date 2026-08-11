[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Version,

    [Parameter(Mandatory = $true)]
    [string]$SourceExe,

    [Parameter(Mandatory = $true)]
    [string]$OutputMsi
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$packageVersion = $Version.TrimStart('v') -replace '-.*$', ''
if ($packageVersion -notmatch '^\d+\.\d+\.\d+$') {
    throw "Version '$Version' does not produce a valid three-part MSI version."
}

$sourceExePath = (Resolve-Path -LiteralPath $SourceExe).Path
$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$licensePath = Join-Path $repositoryRoot 'LICENSE'
if (-not (Test-Path -LiteralPath $licensePath -PathType Leaf)) {
    throw "License file was not found at '$licensePath'."
}

$outputPath = [System.IO.Path]::GetFullPath($OutputMsi)
$outputDirectory = Split-Path -Parent $outputPath
New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null

$workDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ("stateroot-msi-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $workDirectory | Out-Null

try {
    # WixUI uses a RichText control. Generate its RTF from the authoritative
    # repository LICENSE so the installer cannot drift to a stale license copy.
    $licenseText = Get-Content -LiteralPath $licensePath -Raw
    $licenseRtfText = $licenseText.Replace('\', '\\').Replace('{', '\{').Replace('}', '\}')
    $licenseRtfText = $licenseRtfText -replace "`r?`n", "\par`r`n"
    $licenseRtf = "{\rtf1\ansi\deff0{\fonttbl{\f0 Segoe UI;}}\fs18 $licenseRtfText}"
    $licenseRtfPath = Join-Path $workDirectory 'LICENSE.rtf'
    Set-Content -LiteralPath $licenseRtfPath -Value $licenseRtf -Encoding Ascii
    $intermediateDirectory = Join-Path $workDirectory 'obj'
    New-Item -ItemType Directory -Path $intermediateDirectory | Out-Null

    $packageSource = Join-Path $PSScriptRoot 'Package.wxs'
    & wix build $packageSource `
        -arch x64 `
        -ext WixToolset.UI.wixext `
        -ext WixToolset.Util.wixext `
        -d "ProductVersion=$packageVersion" `
        -d "SourceExe=$sourceExePath" `
        -d "LicenseRtf=$licenseRtfPath" `
        -intermediateFolder $intermediateDirectory `
        -pdbtype none `
        -out $outputPath

    if ($LASTEXITCODE -ne 0) {
        throw "WiX failed with exit code $LASTEXITCODE."
    }

    if (-not (Test-Path -LiteralPath $outputPath -PathType Leaf)) {
        throw "WiX completed without producing '$outputPath'."
    }
} finally {
    Remove-Item -LiteralPath $workDirectory -Recurse -Force -ErrorAction SilentlyContinue
}
