# SRun3K login protocol

How the Tsinghua campus portal (`auth4.tsinghua.edu.cn`, SRun3K) is driven from plain HTTP(S) — no browser, no JavaScript.

This document describes what the client implements. The code lives in `src/srun.rs` (protocol) and `src/session.rs` (state machine); the crypto primitives are in `src/crypto.rs`.

Installation, usage and troubleshooting are covered in the [README](../README.md).

**Contents**

1. [Decide whether a login is needed](#1-decide-whether-a-login-is-needed)
2. [Find the `ac_id`](#2-find-the-ac_id)
3. [Fetch a challenge token](#3-fetch-a-challenge-token)
4. [Encode the password](#4-encode-the-password)
5. [Build the `info` blob (`{SRBX1}`)](#5-build-the-info-blob-srbx1)
6. [Sign the request](#6-sign-the-request)
7. [Submit](#7-submit)
8. [Verify the session](#8-verify-the-session)
9. [Stay online](#9-stay-online)
10. [Error codes](#10-error-codes)

## 1. Decide whether a login is needed

Three tiny probe requests (3 s timeout) classify the current network:

| Probe result | Meaning | Reaction |
| --- | --- | --- |
| nothing answers | no link, or a gateway that drops pre-login traffic | wait / log in, see below |
| answer contains `index_<ac_id>.html` | the campus gateway is intercepting HTTP | remember `ac_id`, log in |
| a small HTML/JS stub without `ac_id` | some other captive portal | wait 60 s, retry |
| the real page (or any HTTP status) | nothing is intercepting: the session is up, or this link routes without the portal | no login: there is nothing to address it to |

Two shortcuts are applied first:

- **Link check.** `link_up()` performs a UDP `connect()` to `223.5.5.5:53`: the kernel does a route lookup and sends nothing. Without a route the loop sleeps instead of firing HTTP requests, so a laptop on a train stays quiet.
- **Enterprise Wi-Fi.** SSIDs such as `Tsinghua-Secure` authenticate with 802.1X and never show a captive portal, so the portal login is skipped (`src/wifi.rs`). Reading the SSID requires `netsh` on Windows, which needs Location permission; the check is best-effort, and the "internet works" probe above is the fallback.

Two local facts only ever *upgrade* the "nothing answers" case, where the login is the only way forward:

- **Campus SSID.** Every other `Tsinghua…` SSID (`Tsinghua`, `Tsinghua-IPv4`) keeps the portal, so the name makes a login meaningful there. The name is read from `netsh`, or from the connected network profiles when `netsh` is refused.
- **The portal knows this address.** `/cgi-bin/rad_user_info` reports the caller's own address — as `client_ip` on the older answers and as `online_ip` on the current portal, which has no `client_ip` field at all. On the campus network it equals the address this machine holds on its default route, because nothing is NATed in between; from any other network the two differ. That separates "the campus gateway just is not intercepting" from "this is not our network at all", so a hotel Wi-Fi never gets a login attempt.

A link that already reaches the internet is **not** upgraded, even when the name says campus: there is nothing to log in to, and `ac_id` is per access network — an id borrowed from the link this machine came from makes the portal answer `E2808` or `INFO failed, BAS respond timeout`. Those two answers are classified as "this `ac_id` cannot be used here", which stops the retry loop instead of hammering the portal (see §10).

## 2. Find the `ac_id`

The BRAS access-controller id is **not** a constant. While you are unauthenticated, the gateway intercepts an *external* HTTP request and answers with a stub that points at `<auth4>/index_<ac_id>.html` (for example `index_169.html`). That page name is the authoritative value.

Do **not** read it from `auth4.tsinghua.edu.cn` or `login.tsinghua.edu.cn/index_1.html`: those pages always hardcode the default `1`, and logging in with `ac_id=1` fails with `E2808 Control policy not found`. The client only falls back to them when no interception page is available, and logs a warning when it does.

## 3. Fetch a challenge token

```
GET /cgi-bin/get_challenge?username=<user>&ip=<local-ip>&double_stack=1&callback=C_a_l_l_b_a_c_k
```

The response is JSONP: `C_a_l_l_b_a_c_k({ ... })`. The `challenge` field is a random token used both as the XXTEA key and as the salt of the checksum. `ip` may be empty, in which case the portal infers the caller address.

## 4. Encode the password

```
password = "{MD5}" + md5_hex(plain_password)
```

The password is still sent, but only as a checksum input — the readable form travels inside the encrypted `info` blob below.

## 5. Build the `info` blob (`{SRBX1}`)

`info` is a compact JSON object

```json
{"username":"1234567890","password":"<plain>","ip":"192.0.2.10","acid":"169","enc_ver":"srun_bx1"}
```

which is then

1. encrypted with **XXTEA** (`xencode`, delta `0x9E3779B9`, using the raw bytes of the challenge token as the key),
2. base64-encoded with the portal's own alphabet `LVoJPiCN2R8G90yg+hmFHuacZ1OWMnrsSTXkYpUq/3dlbfKwv6xztjI7DeBE45QA` instead of the standard one,
3. prefixed with the literal `{SRBX1}`.

`src/crypto.rs` implements both steps and is pinned by the five published SRun test vectors plus an MD5 vector (`cargo test`).

## 6. Sign the request

```
chksum = sha1_hex(
  token + username +
  token + md5_password +
  token + ac_id +
  token + ip +
  token + "200" +        // n, client type: PC
  token + "1" +          // type: web login
  token + info           // the "{SRBX1}..." string
)
```

The token separates every field, so a single changed byte invalidates the whole request.

## 7. Submit

```
GET /cgi-bin/srun_portal?action=login&username=..&password={MD5}..&ac_id=..
    &ip=..&n=200&type=1&double_stack=1&info={SRBX1}..&chksum=..&callback=C_a_l_l_b_a_c_k
```

Logout uses the same endpoint with `action=logout`; `password` is dropped from both the query and the `info` blob, and the checksum loses the `token + md5_password` term.

`username` is still mandatory: `get_challenge` answers `missing_required_parameters_error` when it is empty, so a client that did not create the session reads `user_name` (and `online_ip`) from `/cgi-bin/rad_user_info` first. The portal's own page instead sends an unsigned `action=logout&username=..&ip=..&ac_id=..`, but that form returns `no_username_error` without an account name as well.

The account reported by `rad_user_info` therefore takes precedence over any saved credential: on a shared machine the session may belong to a different account than the one stored, and it is that account which has to be signed out. The saved name is only used when the portal reports no session.

A successful answer looks like `{"error":"ok","error_msg":"","suc_msg":"login_ok",...}`.

## 8. Verify the session

The RADIUS accounting row appears roughly **20 seconds** after an accepted login, so `srun_portal` returning `ok` is not proof of success. The client

1. reads the assigned address from `/srun_portal_pc?ac_id=<ac_id>` (that page contains the client's public IP),
2. asks `/cgi-bin/rad_user_info?ip=<ip>`, which returns the live session: `online`, `user_ip`, `online_ip`, `sum_bytes`, `sum_seconds`, `user_name`,
3. polls step 2 once per second for up to ~30 s (`wait_online`).

`rad_user_info` also feeds the traffic counters and the periodic status refresh (every 15 s while online). The per-device list in `online_device_detail` arrives as a JSON *string* (an object encoded as text); the client parses it into an object before handing the answer on, and keeps the original text if it is not one.

## 9. Staying online

- **Heartbeat:** the core polls `/srun_portal_pc` and `/cgi-bin/rad_user_info` every 10 s while offline and every 15 s while online. That poll is also the keepalive: `keepalive_time` in the answer always equals the time of the request, so the portal renews the session on it and the client sends no separate keep-alive traffic.
- **Intent:** an automatic login needs the user's runtime intent (seeded by *Connect on startup*, set by a manual connect) and either *Stay connected* or an open startup window. A logout on the portal website is indistinguishable from a kick or a timeout, so *Stay connected* is what decides whether the core signs back in.
- **Watchdog:** when the link drops the UDP route lookup fails and the loop waits instead of sending requests; when the link returns the backoff is reset and a login is attempted within ~1 s.
- **Backoff:** consecutive failures wait `reconnect_delay_secs × 2^(n-1)`, capped at 120 s. A rejected password is not retried automatically (the same pair can only be refused again), and neither is an `ac_id` the portal cannot route: both drop the intent, so every automatic path stops until a manual connect.

## 10. Error codes

| Code | Typical cause |
| --- | --- |
| `E2808 Control policy not found` | wrong `ac_id` (usually a hardcoded `1`) |
| `E2901` / `password error` | wrong password, or a stale `{MD5}` digest |
| `E2553` | account/IP mismatch (already logged in elsewhere) |
| `E2620 You are already online` | the account still has a session - on another device, or on the link this machine just left (an 802.1X SSID creates one too); no retry opens a second session, so the client stops and says so |
