# Captures what THUnet decides on whatever network this machine is on.
#
# Run it once per link (`-Label ipv4`, `-Label ipv6`, ...) and compare the files:
# it records the adapter/route/DNS facts the app reasons about, the two route
# lookups the app itself performs (IPv4 and IPv6), a few short HTTP probes with
# their remote address, and a time series of the running core's verdict.
#
# It changes nothing: no install, no config write, no login. Requests go to the
# portal and to the three probe URLs the app already uses, and each has a short
# timeout, so it finishes on a link where nothing answers.
#
#   pwsh -NoProfile -File scripts/net-report.ps1 -Label ipv4
#
# The report lands in dist/ (ignored by git) and its path is printed at the end.

param(
    [string]$Label = "",
    [int]$Seconds = 20,
    [int]$Step = 5,
    [string]$Exe = "$env:ProgramFiles\THUnet\bin\thunet.exe"
)

$ErrorActionPreference = "Continue"
# Response bodies are UTF-8 (the portal's JSON carries Chinese); without this,
# PowerShell decodes a native command's output with the OEM code page and every
# Chinese field in the report turns to mojibake.
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$suffix = if ($Label) { "-$Label" } else { "" }
$out = Join-Path $dist "net-report-$stamp$suffix.txt"

$lines = New-Object System.Collections.Generic.List[string]
function Say([string]$text = "") { $lines.Add($text) }

# The same route lookup the app performs: connect a datagram socket, read back
# the local address the kernel chose. Nothing is sent.
function Route-Lookup([string]$target, [string]$family) {
    try {
        $af = if ($family -eq "v6") {
            [System.Net.Sockets.AddressFamily]::InterNetworkV6
        } else {
            [System.Net.Sockets.AddressFamily]::InterNetwork
        }
        $u = New-Object System.Net.Sockets.UdpClient -ArgumentList 0, $af
        try {
            $u.Connect($target, 53)
            $local = $u.Client.LocalEndPoint.ToString()
        } finally {
            $u.Close()
        }
        if ($local.StartsWith("0.0.0.0:") -or $local.StartsWith("[::]:")) { "none ($local)" } else { $local }
    } catch {
        # The reason is a Windows message in the machine's language; it is not
        # worth the mojibake here, and "no route" is the only thing it means.
        "none (no route)"
    }
}

function Probe-Url([string]$url, [string[]]$force = @(), [int]$timeout = 8) {
    $a = @('-sS', '-o', 'NUL', '-w', '%{http_code} ip=%{remote_ip} t=%{time_total}s')
    $a += $force
    $a += @('--max-time', "$timeout", $url)
    $result = & curl.exe @a 2>&1
    if ($LASTEXITCODE -ne 0) { "FAILED: $($result -join ' ')" } else { $result }
}

# The same request, with the address given as a literal: `--resolve` keeps the
# host name for the Host header and the certificate, but skips getaddrinfo. On a
# link where name resolution through the Windows resolver is broken this is the
# only way to tell "the portal does not answer" from "nothing can be resolved".
function Probe-Pinned([string]$url, [string]$resolve, [int]$timeout = 8, [int]$body = 0) {
    $sink = if ($body -gt 0) { '-' } else { 'NUL' }
    $a = @('-sS', '-o', $sink, '-w', '%{http_code} ip=%{remote_ip} t=%{time_total}s',
        '--resolve', $resolve, '--max-time', "$timeout", $url)
    $result = (& curl.exe @a 2>&1) -join ' '
    if ($body -gt 0 -and $LASTEXITCODE -eq 0) {
        $result = $result -replace '\s+', ' '
        if ($result.Length -gt $body) { $result = $result.Substring(0, $body) + '...' }
    }
    if ($LASTEXITCODE -ne 0) { "FAILED: $result" } else { $result }
}

function Resolve-First([string]$name, [string]$type) {
    $a = Resolve-DnsName -Name $name -Type $type -DnsOnly -QuickTimeout -ErrorAction SilentlyContinue
    if ($a) { ($a | Where-Object { $_.Type -eq $type } | Select-Object -First 1 -ExpandProperty IPAddress) } else { '' }
}

Say "THUnet network report"
Say "label    : $(if ($Label) { $Label } else { '(none)' })"
Say "started  : $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss K')"
Say "host     : $env:COMPUTERNAME"
Say "exe      : $Exe"
if (Test-Path $Exe) {
    $f = Get-Item $Exe
    Say "exe built: $($f.LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss')), $([int]($f.Length / 1MB)) MB"
} else {
    Say "exe built: (not found)"
}
Say ""

Say "=== adapters (up) ==="
Get-NetAdapter | Where-Object Status -eq 'Up' |
    ForEach-Object { Say ("  {0,-28} {1}" -f $_.Name, $_.InterfaceDescription) }
Say ""

Say "=== addresses ==="
Say "  IPv4:"
$v4 = Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -notlike '127.*' }
if ($v4) {
    $v4 | ForEach-Object {
        Say ("    {0,-18} /{1,-3} if={2} origin={3}" -f $_.IPAddress, $_.PrefixLength, $_.InterfaceAlias, $_.PrefixOrigin)
    }
} else {
    Say "    (none)"
}
Say "  IPv6 (global and unique-local):"
$v6 = Get-NetIPAddress -AddressFamily IPv6 | Where-Object {
    $_.IPAddress -notlike 'fe80::*' -and $_.IPAddress -ne '::1'
}
if ($v6) {
    $v6 | ForEach-Object {
        Say ("    {0}" -f $_.IPAddress)
        Say ("      /{0} if={1} origin={2} suffix={3}" -f $_.PrefixLength, $_.InterfaceAlias, $_.PrefixOrigin, $_.SuffixOrigin)
    }
} else {
    Say "    (none)"
}
Say ""

Say "=== default routes ==="
$r4 = Get-NetRoute -AddressFamily IPv4 -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue
if ($r4) {
    $r4 | ForEach-Object { Say ("  v4  0.0.0.0/0 -> {0} if={1} metric={2}" -f $_.NextHop, $_.InterfaceAlias, $_.RouteMetric) }
} else {
    Say "  v4  (none)"
}
$r6 = Get-NetRoute -AddressFamily IPv6 -DestinationPrefix '::/0' -ErrorAction SilentlyContinue
if ($r6) {
    $r6 | ForEach-Object { Say ("  v6  ::/0 -> {0} if={1} metric={2}" -f $_.NextHop, $_.InterfaceAlias, $_.RouteMetric) }
} else {
    Say "  v6  (none)"
}
Say ""

Say "=== DNS servers ==="
Get-DnsClientServerAddress | Where-Object { $_.ServerAddresses } |
    ForEach-Object { Say ("  {0,-24} {1}  {2}" -f $_.InterfaceAlias, $_.AddressFamily, ($_.ServerAddresses -join ', ')) }
Say ""

Say "=== the app's own route lookups ==="
$look4 = Route-Lookup '223.5.5.5' 'v4'
$look6 = Route-Lookup '2400:3200::1' 'v6'
Say "  link_id()  (IPv4, 223.5.5.5:53)          : $look4"
Say "  link_id6() (IPv6, 2400:3200::1:53)       : $look6"
$up = (-not $look4.StartsWith('none')) -or (-not $look6.StartsWith('none'))
Say "  -> link_up() is $(if ($up) { 'TRUE' } else { 'FALSE' }) (either family is a link)"
if (-not $up) {
    Say "  -> the core reads 'no link': the probe, the poll, the login and the"
    Say "     window's own verdict all stop here"
}
Say ""

Say "=== name resolution ==="
foreach ($q in @(@('auth4.tsinghua.edu.cn', 'A'), @('auth4.tsinghua.edu.cn', 'AAAA'), @('www.tsinghua.edu.cn', 'AAAA'))) {
    $host_, $type = $q
    $answer = Resolve-DnsName -Name $host_ -Type $type -ErrorAction SilentlyContinue -QuickTimeout
    if ($answer) {
        $answer | Where-Object { $_.Type -eq $type } | ForEach-Object {
            Say ("  {0} {1}: {2}" -f $host_, $type, $_.IPAddress)
        }
        if (-not ($answer | Where-Object { $_.Type -eq $type })) { Say "  $host_ $type : (no $type record)" }
    } else {
        Say "  $host_ $type : (lookup failed)"
    }
}
Say ""

Say "=== name resolution through getaddrinfo (what the app and curl use) ==="
foreach ($name in @('www.baidu.com', 'auth4.tsinghua.edu.cn', 'auth.tsinghua.edu.cn')) {
    try {
        $r = [System.Net.Dns]::GetHostAddresses($name)
        Say ("  {0,-24} {1}" -f $name, (($r | ForEach-Object { $_.IPAddressToString }) -join ', '))
    } catch {
        $why = $_.Exception.Message
        if ($_.Exception.InnerException) { $why = $_.Exception.InnerException.Message }
        Say ("  {0,-24} FAILED - {1}" -f $name, $why)
    }
}
Say ""

Say "=== portal over each family, address pinned (no getaddrinfo) ==="
$v4 = Resolve-First 'auth4.tsinghua.edu.cn' 'A'
$v6 = Resolve-First 'auth.tsinghua.edu.cn' 'AAAA'
Say "  auth4.tsinghua.edu.cn  A    : $(if ($v4) { $v4 } else { '(none)' })"
Say "  auth.tsinghua.edu.cn   AAAA : $(if ($v6) { $v6 } else { '(none)' })"
$https4 = 'https://auth4.tsinghua.edu.cn/cgi-bin/rad_user_info?callback=cb'
$https6 = 'https://auth.tsinghua.edu.cn/cgi-bin/rad_user_info?callback=cb'
$http6 = 'http://auth.tsinghua.edu.cn/'
if ($v4) {
    Say "  https v4 : $(Probe-Pinned $https4 "auth4.tsinghua.edu.cn:443:$v4" 8 200)"
}
if ($v6) {
    Say "  https v6 : $(Probe-Pinned $https6 "auth.tsinghua.edu.cn:443:[$v6]" 8 200)"
    Say "  http  v6 : $(Probe-Pinned $http6 "auth.tsinghua.edu.cn:80:[$v6]" 8 200)"
} else {
    Say "  https v6 : (no AAAA for the portal, not even attempted)"
}
# Any v6 at all: a campus host that does publish an AAAA. Without this, a
# refused portal connection cannot be told apart from a link that carries no v6.
$web6 = Resolve-First 'www.tsinghua.edu.cn' 'AAAA'
if ($web6) {
    Say "  www.tsinghua.edu.cn v6 : $(Probe-Pinned 'https://www.tsinghua.edu.cn/' "www.tsinghua.edu.cn:443:[$web6]" 8 100)"
}
Say "  (no pins, plain name lookup) : $(Probe-Url $https4 @() 5)"
Say ""

Say "=== the three probes net_env() uses ==="
foreach ($p in @(
        'http://connect.rom.miui.com/generate_204',
        'http://www.msftconnecttest.com/connecttest.txt',
        'http://www.baidu.com/')) {
    Say "  $p"
    Say "    default : $(Probe-Url $p @() 5)"
    Say "    forced v4: $(Probe-Url $p @('-4') 5)"
    Say "    forced v6: $(Probe-Url $p @('-6') 5)"
}
Say ""

Say "=== the core's verdict, one line every $Step s for $Seconds s ==="
Say "  (link = the interface row, state = session, portal = ours/foreign/silent)"
$deadline = (Get-Date).AddSeconds($Seconds)
while ((Get-Date) -lt $deadline) {
    $at = Get-Date -Format 'HH:mm:ss'
    # stderr carries the portal's own complaint when the poll fails; it is not
    # JSON and would swallow the answer it belongs next to.
    $raw = (& $Exe --timeout 2 status 2>$null | Out-String)
    try {
        $o = $raw | ConvertFrom-Json -ErrorAction Stop
        $c = $o.core
        $portal = ($c.env.portal | ConvertTo-Json -Compress -Depth 4)
        Say ("  {0} link={1,-9} state={2,-9} connected={3,-5} internet={4,-5} portal={5} os_session={6} ac_id={7} failures={8}" -f `
                $at, $c.link, $c.state, $c.env.connected, $c.env.internet, $portal, $c.env.os_session, $c.ac_id, $c.failures)
        if ($o.portal_error) { Say ("           portal_error: {0}" -f $o.portal_error) }
    } catch {
        Say ("  {0} status: unreadable ({1} bytes)" -f $at, $raw.Length)
    }
    Start-Sleep -Seconds $Step
}
Say ""

Say "=== core log tail ==="
$log = Join-Path $env:ProgramData 'thunet\core.log'
$tail = $null
$why = "($log does not exist)"
try {
    $null = Get-Item $log -ErrorAction Stop
    $tail = Get-Content $log -Tail 120 -ErrorAction Stop
} catch [System.IO.FileNotFoundException] {
    $why = "($log does not exist)"
} catch [System.UnauthorizedAccessException] {
    $why = "(cannot read ${log}: access denied, run this from an elevated window to include it)"
} catch {
    $why = "(cannot read ${log}: $($_.Exception.GetType().Name))"
}
if ($tail) {
    $tail | ForEach-Object { Say "  $_" }
} else {
    Say "  $why"
}
Say ""

Say "finished : $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss K')"

Set-Content -Path $out -Value $lines -Encoding UTF8
$lines | ForEach-Object { $_ }
Write-Host ""
Write-Host "report: $out"
