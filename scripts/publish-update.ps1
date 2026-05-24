#requires -Version 5.1
<#
.SYNOPSIS
    Signs a Tauri NSIS bundle and emits the updater manifest (latest.json).

.DESCRIPTION
    Run after `pnpm desktop:build:business` or `pnpm desktop:build:admin`.
    Locates the .nsis.zip bundle, signs it with the Tauri private key, and writes
    `latest.json` describing the release. Uploading the artifacts to the CDN is
    your job — this script does NOT push anything.

.PARAMETER App
    "business" or "admin".

.PARAMETER Version
    Semver string matching tauri.<app>.conf.json `version`, e.g. "1.0.1".

.PARAMETER Notes
    Release notes shown to users by the updater.

.PARAMETER PrivateKey
    Path to the Tauri private key (generated via `pnpm tauri signer generate`).
    Defaults to $env:TAURI_SIGNING_PRIVATE_KEY.

.PARAMETER PrivateKeyPassword
    Password for the private key. Defaults to $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD.

.PARAMETER BaseUrl
    Base URL prefix used inside latest.json. Default points at the GitHub
    Releases pattern; pass a different value if you self-host.

.EXAMPLE
    .\publish-update.ps1 -App business -Version 1.0.1 -Notes "Bug fixes."
#>
param(
    [Parameter(Mandatory)] [ValidateSet("business", "admin")] [string]$App,
    [Parameter(Mandatory)] [string]$Version,
    [string]$Notes = "Improvements and fixes.",
    [string]$PrivateKey = $env:TAURI_SIGNING_PRIVATE_KEY,
    [string]$PrivateKeyPassword = $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD,
    [string]$BaseUrl = "https://github.com/LogixOrg/orcaa-desktop/releases/download/v$Version"
)

$ErrorActionPreference = "Stop"

if (-not $PrivateKey) {
    throw "Private key path missing. Pass -PrivateKey or set TAURI_SIGNING_PRIVATE_KEY."
}
if (-not (Test-Path $PrivateKey)) {
    throw "Private key not found at: $PrivateKey"
}

$RepoRoot = (Resolve-Path "$PSScriptRoot\..").Path
$BundleDir = Join-Path $RepoRoot "src-tauri\target\release\bundle\nsis"
$OutDir = Join-Path $RepoRoot "dist\$App"

if (-not (Test-Path $BundleDir)) {
    throw "Bundle directory missing: $BundleDir`nRun 'pnpm build:$App' first."
}

$ZipPattern = "*_${Version}_x64-setup.nsis.zip"
$Bundle = Get-ChildItem -Path $BundleDir -Filter $ZipPattern | Select-Object -First 1
if (-not $Bundle) {
    throw "No bundle found matching $ZipPattern in $BundleDir."
}

Write-Host "Signing $($Bundle.Name)..." -ForegroundColor Cyan

$env:TAURI_SIGNING_PRIVATE_KEY = $PrivateKey
if ($PrivateKeyPassword) {
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $PrivateKeyPassword
}

& pnpm tauri signer sign -k $PrivateKey $Bundle.FullName
if ($LASTEXITCODE -ne 0) {
    throw "tauri signer sign failed (exit $LASTEXITCODE)."
}

$SigFile = "$($Bundle.FullName).sig"
if (-not (Test-Path $SigFile)) {
    throw "Signature file not produced: $SigFile"
}

$Signature = (Get-Content $SigFile -Raw).Trim()

if (-not (Test-Path $OutDir)) {
    New-Item -ItemType Directory -Path $OutDir -Force | Out-Null
}

Copy-Item -Path $Bundle.FullName -Destination $OutDir -Force
Copy-Item -Path $SigFile -Destination $OutDir -Force

$ExePattern = "*_${Version}_x64-setup.exe"
$MsiPattern = "*_${Version}_x64_en-US.msi"
$ExeFile = Get-ChildItem -Path $BundleDir -Filter $ExePattern | Select-Object -First 1
$MsiDir = Join-Path $RepoRoot "src-tauri\target\release\bundle\msi"
$MsiFile = if (Test-Path $MsiDir) { Get-ChildItem -Path $MsiDir -Filter $MsiPattern | Select-Object -First 1 } else { $null }

if ($ExeFile) { Copy-Item -Path $ExeFile.FullName -Destination $OutDir -Force }
if ($MsiFile) { Copy-Item -Path $MsiFile.FullName -Destination $OutDir -Force }

$Manifest = [ordered]@{
    version = $Version
    notes = $Notes
    pub_date = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    platforms = [ordered]@{
        "windows-x86_64" = [ordered]@{
            signature = $Signature
            url = "$BaseUrl/$($Bundle.Name)"
        }
    }
}

$ManifestPath = Join-Path $OutDir "latest.json"
$Manifest | ConvertTo-Json -Depth 10 | Out-File -FilePath $ManifestPath -Encoding utf8

Write-Host ""
Write-Host "Artifacts staged in: $OutDir" -ForegroundColor Green
Write-Host "Next step: create a GitHub release at https://github.com/LogixOrg/orcaa-desktop/releases tagged v$Version,"
Write-Host "then upload all files from $OutDir as release assets."
Write-Host ""
Get-ChildItem -Path $OutDir | Select-Object Name, Length | Format-Table
