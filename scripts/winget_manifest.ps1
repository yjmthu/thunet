<#
.SYNOPSIS
  Generate winget manifests for the packaged MSI.

.DESCRIPTION
  Reads the version from Cargo.toml, hashes the MSI produced by
  `pixi run -e package msi`, and writes the three manifest files that winget
  needs (version / installer / default locale) into dist/winget/.

  Local testing:
      winget validate --manifest dist/winget

  Publishing to the community repository:
      1. upload the MSI to a release and pass its URL as -BaseUrl
      2. wingetcreate submit --token <GitHub token> dist/winget
     (or open a PR with the files copied to
      manifests/t/THUnet/THUnet/<version>/ in microsoft/winget-pkgs)

.PARAMETER BaseUrl
  URL of the directory that holds the MSI, e.g.
  https://github.com/<owner>/thunet/releases/download/v<version>

.PARAMETER License
  SPDX license shown in winget; must match what you actually ship.

.PARAMETER Msi
  Path to the MSI. Defaults to dist/thunet-<version>-x64.msi.
#>
param(
  [string]$BaseUrl = "",
  [string]$License = "MIT",
  [string]$Msi = ""
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$manifestVersion = "1.10.0"
$identifier = "THUnet.THUnet"

$cargo = Get-Content (Join-Path $root "Cargo.toml") -Raw
$match = [regex]::Match($cargo, '(?m)^version\s*=\s*"([^"]+)"')
if (-not $match.Success) { throw "cannot read version from Cargo.toml" }
$version = $match.Groups[1].Value

if (-not $Msi) { $Msi = Join-Path $root "dist/thunet-$version-x64.msi" }
if (-not (Test-Path $Msi)) {
  throw "MSI not found: $Msi`nRun: pixi run -e package msi"
}
$fileName = Split-Path -Leaf $Msi
$hash = (Get-FileHash -Path $Msi -Algorithm SHA256).Hash

if (-not $BaseUrl) {
  $BaseUrl = "https://github.com/OWNER/thunet/releases/download/v$version"
  Write-Warning "No -BaseUrl supplied; the manifests point at a placeholder URL."
  Write-Warning "Replace OWNER (or re-run with -BaseUrl) before submitting."
}
$url = "$BaseUrl/$fileName"

$outDir = Join-Path $root "dist/winget"
# Start clean so stale manifests from an older package name/version cannot be
# validated or submitted by accident.
if (Test-Path $outDir) { Remove-Item -Recurse -Force $outDir }
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$versionYaml = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.version.$manifestVersion.schema.json
PackageIdentifier: $identifier
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: $manifestVersion
"@

$installerYaml = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.installer.$manifestVersion.schema.json
PackageIdentifier: $identifier
PackageVersion: $version
Platform:
- Windows.Desktop
MinimumOSVersion: 10.0.17763.0
InstallerType: msi
Scope: user
InstallModes:
- interactive
- silent
- silentWithProgress
UpgradeBehavior: install
Installers:
- Architecture: x64
  InstallerUrl: $url
  InstallerSha256: $hash
  Scope: user
  InstallerSwitches:
    Silent: /qn
    SilentWithProgress: /qb
ManifestType: installer
ManifestVersion: $manifestVersion
"@

$localeYaml = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.defaultLocale.$manifestVersion.schema.json
PackageIdentifier: $identifier
PackageVersion: $version
PackageLocale: en-US
Publisher: THUnet contributors
PackageName: THUnet
License: $License
ShortDescription: Tray client for the Tsinghua University campus network.
Description: Logs in to the Tsinghua campus network (SRun portal), keeps the
  session alive, shows the assigned IPv4/IPv6 address and traffic, and supports
  autostart. Per-user install, no administrator rights required.
Moniker: thunet
Tags:
- tsinghua
- campus
- network
- srun
- captive-portal
ManifestType: defaultLocale
ManifestVersion: $manifestVersion
"@

Set-Content -Path (Join-Path $outDir "$identifier.yaml") -Value $versionYaml -Encoding utf8
Set-Content -Path (Join-Path $outDir "$identifier.installer.yaml") -Value $installerYaml -Encoding utf8
Set-Content -Path (Join-Path $outDir "$identifier.locale.en-US.yaml") -Value $localeYaml -Encoding utf8

Write-Host "wrote $outDir"
Write-Host "  version   = $version"
Write-Host "  installer = $url"
Write-Host "  sha256    = $hash"
