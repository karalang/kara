# mini_fail2ban

fail2ban is the intrusion-prevention daemon on most Linux servers. It
reads auth, web and mail logs, matches each line against per-service
regular expressions, and bans an address with a firewall rule once it
fails too often. The program ports its configuration, its filters, its
date detector and its ban logic, over `std.regex`. It prints what the
same jails print in `oracle.py`, which runs them in Python with `re`.

The run writes into `data/`:
- `jail.local`, with a `[DEFAULT]` section and six jails: `sshd`,
  `nginx-http-auth` and `postfix-sasl`, one disabled jail, one whose
  filter file is missing, and one whose log is missing;
- `filter.d/common.conf` and four filters. `sshd`, `postfix-sasl` and
  `vsftpd` include `common.conf` with `[INCLUDES] before =`;
- `log/auth.log`, `log/nginx/error.log` and `log/mail.log`.

**Configuration.** A jail's settings override `[DEFAULT]`, including
`ignoreip`, so `postfix-sasl` does not ignore 127.0.0.1. Lines starting
with `#` or `;` are comments, and an indented line continues the value
above it, which is how a filter lists several `failregex` lines.

**Filters.** `%(name)s` is replaced from the filter's `[Definition]`,
then from the included `[DEFAULT]`, recursively, so `__prefix_line` in
`common.conf` picks up each filter's `_daemon`. `<HOST>` becomes a
named group that matches an IPv4 or IPv6 address. Each expanded pattern
is printed, then compiled once.

**Dates.** A syslog stamp (`Oct 17 03:00:15`, year 2026) or an nginx
stamp (`2026/10/17 04:00:10`) is matched at the start of the line and
removed before the filters run, as fail2ban's date detector does. A line
with neither is counted and skipped.

**Bans.**
- The first `failregex` that matches gives the host.
- Then `ignoreregex` can drop the line, and `ignoreip` can ignore the
  host, by exact address or by IPv4 CIDR range.
- Failures older than `findtime` are forgotten. At `maxretry` failures
  the host is banned and its failures cleared.
- With `bantime.increment`, ban *n* lasts `bantime * 2^(n-1)`, capped
  at `bantime.maxtime`.
- Bans that ran out are lifted before each line, earliest first.
- A failure from a banned host is reported as already banned.

After each jail comes its `fail2ban-client status` block. The bans still
active are written as `iptables` or `ip6tables` commands to
`data/action/<jail>.sh`, read back and printed, and lifted at shutdown.

`mk.py` holds the input files. `gen_kara.py` prints them as the Kāra
data functions in the program.

## Edits from mini_fail2ban.orig.kara to mini_fail2ban.kara

These were author errors, not compiler gaps:
- `!x` instead of `not x` (five places; `karac fix` rewrote them).
- `mut self` instead of `mut ref self` on `Section.set`.
- The `set` parameters were bare `String`, which borrows; storing them
  needs `own String`.
- `s.parse[i64]()` instead of `i64.parse(s)`.
- `as_bytes()` instead of `bytes()`.
- `bytes().all(..)` and `bytes().filter(..)` without `.iter()`.
- `.clone()` on `Section` without `#[derive(Clone)]`.

This one is a workaround for a gap:
- `path.rfind("/")` became the `parent` helper (B-2026-10-15-15,
  strings.md has no `rfind`).

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
