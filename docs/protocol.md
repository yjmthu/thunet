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

Three tiny probe requests (2 s timeout) say what is on the path to the internet:

| Probe result | Meaning |
| --- | --- |
| nothing answers | no link, broken DNS, or a gateway that drops pre-login traffic |
| answer contains `index_<ac_id>.html` | the campus gateway is intercepting HTTP: remember `ac_id`, log in |
| a small HTML/JS stub without `ac_id` | another operator's captive portal |
| the real page (or any HTTP status) | nothing is intercepting |

The client acts on a *verdict* of four separate facts (`LinkEnv` in `src/proto.rs`), not on one flat list of network cases:

- **Is there a link at all.** `link_up()` performs a UDP `connect()` to `223.5.5.5:53`: the kernel does a route lookup and sends nothing. Without one the loop sleeps instead of firing HTTP requests, so a laptop on a train stays quiet.
- **Does the internet work.** The probes above reached the internet without being intercepted. This is what tells "the gateway is holding me back" (log in) from "this link simply routes" (nothing to do).
- **Is the campus portal behind it.** Either the interception named the `ac_id`, or `/cgi-bin/rad_user_info` answered. That is the property every campus path shares — cable, campus Wi-Fi, a router, a VPN, an SSL VPN into the campus — and none of them has to carry a campus SSID, so the Wi-Fi name is never consulted. A portal answer is an answer whether or not it names a session (`error: "ok"` vs `not_online_error`).
- **Does the OS own the session.** The portal names the service a session was authenticated against in `domain` (`tsinghua-secure_new` for the 802.1X login), and a session from that service belongs to the OS: its row is a mirror of it, so no portal verb applies. This is a *portal* answer rather than a local one on purpose — a phone sharing its own 802.1X session to this machine would otherwise have the computer read the phone's Wi-Fi name.

A login is attempted only where it can work: the portal is ours, the gateway is holding traffic back, and the OS does not own the session. Where the portal answers but the link routes anyway — an SSL VPN's egress, a router that is already signed in — the client reads the session and signs nothing in: that address belongs to the gateway, and a login would vouch for everybody behind it.

The verdict is **not polled**. It is re-derived when something changes: the link watch sees a new default-route address or a new interface, the route *toward the portal* changes (that is how an SSL VPN which leaves the default route alone is noticed — the same `UdpSocket::connect` route lookup, aimed at the portal's address), the user presses *Connect* or *Refresh*, or the window comes to the front. A verdict nobody changed cannot change by itself, and on a phone the difference is every request the client no longer sends.

An `ac_id` is per access network. An id borrowed from the link this machine came from makes the portal answer `E2808` or `INFO failed, BAS respond timeout`; those two are classified as "this `ac_id` cannot be used here", which stops the retry loop instead of hammering the portal (see §10).

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

Every response is decoded by the charset its `Content-Type` names (`srun::decode_body`), and as UTF-8 when the header names none: `rad_user_info` answers `application/json; charset=utf-8`, while some deployments serve the login form as GBK, and reading those bytes as UTF-8 would turn every Chinese field into a replacement character.

All of this describes one link. When the link changes — a cable pulled, Wi-Fi joined, a tunnel that reroutes the portal — the core blanks the portal rows (`Ev::Info({})`) before the next poll rather than leaving the previous link's account, service and addresses on screen under the new interface, then polls again within a second and keeps a few catch-up polls until the portal's own accounting row shows up.

## 9. Staying online

- **Heartbeat:** the core polls `/srun_portal_pc` and `/cgi-bin/rad_user_info` every 10 s while offline and every 15 s while online, on links where the campus portal is actually behind the path. On any other link the poll is skipped — there it is a guaranteed timeout whose answer the probe already has — except while a session is up, where the poll is the only thing watching the link. That poll is also the keepalive: `keepalive_time` in the answer always equals the time of the request, so the portal renews the session on it and the client sends no separate keep-alive traffic.
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
