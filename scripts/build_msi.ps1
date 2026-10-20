#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Build the per-user MSI.

.DESCRIPTION
    The version is never hard-coded: by default it is read from Cargo.toml, and
    -Version overrides it for an ad-hoc build. It is passed to WiX as
    $(var.Version), so the file name, the "Installed apps" entry and the upgrade
    logic all agree.

    Run through pixi so that .NET and WiX are on PATH:

        pixi run -e package msi
        pwsh -File scripts/build_msi.ps1 -Version 0.2.0
#>
param(
    [string]$Version = "",
    [string]$Arch = "x64"
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot

if (-not $Version) {
    $cargo = Get-Content (Join-Path $root "Cargo.toml") -Raw
    $match = [regex]::Match($cargo, '(?m)^version\s*=\s*"([^"]+)"')
    if (-not $match.Success) { throw "cannot read version from Cargo.toml; pass -Version" }
    $Version = $match.Groups[1].Value
}

$wix = Join-Path $root ".pixi/wix/wix.exe"
if (-not (Test-Path $wix)) {
    throw "WiX is not installed yet; run: pixi run -e package wix-tool"
}

$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$out = Join-Path $dist "thunet-$Version-$Arch.msi"

& $wix build (Join-Path $root "packaging/thunet.wxs") `
    -d "Version=$Version" -arch $Arch -o $out
if ($LASTEXITCODE -ne 0) { throw "wix build failed with exit code $LASTEXITCODE" }

Write-Host "built $out"
