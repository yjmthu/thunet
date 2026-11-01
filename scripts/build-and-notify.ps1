# Runs one packaging task and says so when it is done.
#
# The point is a background build that does not finish in silence: the task runs
# with its output in dist/<task>.log, and the last thing it does is a desktop
# notification (see scripts/notify.ps1) carrying the duration, the artifact and
# - when it failed - the line that explains why.
#
#   pwsh -NoProfile -File scripts/build-and-notify.ps1 -Task msi
#   pwsh -NoProfile -File scripts/build-and-notify.ps1 -Task apk-release
#
# The exit code is the task's own, so a caller can still branch on it.

param(
    [Parameter(Mandatory)]
    [ValidateSet('msi', 'msi-debug', 'apk-release', 'apk-debug')]
    [string]$Task,
    # `auto` = toast, with a message box when the toast cannot be shown.
    [ValidateSet('auto', 'toast', 'box')][string]$How = 'auto'
)

$ErrorActionPreference = "Continue"
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$log = Join-Path $dist "$Task.log"

# Which pixi environment owns the task, and where its artifact lands.
switch ($Task) {
    'msi' { $env_name = 'package'; $pattern = 'thunet-*-x64.msi' }
    'msi-debug' { $env_name = 'package'; $pattern = 'thunet-*-x64-debug.msi' }
    'apk-release' { $env_name = 'android'; $pattern = $null }
    'apk-debug' { $env_name = 'android'; $pattern = $null }
}
$apk = if ($env_name -eq 'android') {
    Join-Path $root ("android/app/build/outputs/apk/" +
        $(if ($Task -eq 'apk-release') { "release/app-release.apk" } else { "debug/app-debug.apk" }))
} else {
    $null
}

$started = Get-Date
"=== $Task started $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') ===" | Set-Content -Path $log -Encoding UTF8
& pixi run -e $env_name $Task 2>&1 | Tee-Object -FilePath $log -Append | Out-Host
$code = $LASTEXITCODE
$took = (Get-Date) - $started
$elapsed = "{0}m{1:00}s" -f [int][Math]::Floor($took.TotalMinutes), $took.Seconds
"=== $Task exit status: $code after $elapsed ===" | Add-Content -Path $log -Encoding UTF8

# What the build produced, newest first: the artifact is the thing worth naming.
$artifact = $null
if ($apk) {
    if (Test-Path $apk) { $artifact = Get-Item $apk }
} elseif ($pattern) {
    $artifact = Get-ChildItem (Join-Path $dist $pattern) -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
}

$title = if ($code -eq 0) { "THUnet: $Task built" } else { "THUnet: $Task FAILED" }
if ($code -eq 0 -and $artifact) {
    $size = "{0:N1} MB" -f ($artifact.Length / 1MB)
    $message = "$elapsed - $($artifact.Name) - $size"
} elseif ($code -eq 0) {
    $message = "$elapsed - see dist/$Task.log"
} else {
    $why = Get-Content $log -Tail 40 -ErrorAction SilentlyContinue |
        Where-Object { $_.ToString().Trim() } | Select-Object -Last 1
    if ($why -and $why.Length -gt 140) { $why = $why.Substring(0, 140) + '...' }
    $message = if ($why) { "$elapsed - $why" } else { "$elapsed - see dist/$Task.log" }
}

& (Join-Path $PSScriptRoot 'notify.ps1') -Title $title -Message $message -How $How
exit $code
