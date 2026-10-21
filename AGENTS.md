# Working in this repository

## Never commit real identifiers

Code, tests, documentation, commit messages, screenshots and log samples must not
contain real personal or network identifiers. In particular:

- no real account names or passwords (TUNET or otherwise);
- no real IPv4/IPv6 addresses, MAC addresses, host names or user-profile paths;
- no tokens, API keys, private keys or sealed credentials (`credential.bin`,
  `key.bin`), and no `config.json`, `core.log` or `crash.log` from a real machine.

Use placeholders instead:

| Kind | Value |
| --- | --- |
| Account | `alice`, `bob`, or `1234567890` |
| Password | `pw`, `hunter2` (both are obviously fake) |
| IPv4 | `192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24` (RFC 5737) |
| IPv6 | `2001:db8::/32` (RFC 3849) |
| Local paths | `C:\Users\alice\...`, `/home/alice/...` |

Public service names (`auth4.tsinghua.edu.cn`, `Tsinghua-Secure`) are fine.

Runtime state and build artifacts are ignored: `credential.bin`, `key.bin`,
`config.json`, `core.log`, `crash.log`, `dist/`, `*.msi`, `*.deb`, `*.tar.gz`.
Never force-add them.

Before committing, review `git diff --cached`. If personal data ever lands in a
commit, rewrite the history instead of adding a second commit that deletes it
(a deletion commit leaves the value readable in the old commit).
