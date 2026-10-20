# mini_pyotp

pyotp 2.10.0, the Python library sites and tools use for one-time
passwords (two-factor login codes): HOTP (RFC 4226), TOTP (RFC 6238) and
Steam Guard codes. The port follows the package:
- `HOTP`, `TOTP` and `contrib.Steam` with their keyword arguments,
  including the `TypeError` Python gives a keyword a class does not take;
- `generate_otp` over HMAC with SHA-1, SHA-224, SHA-256, SHA-384 and
  SHA-512, the secret through `base64.b32decode` with its padding and
  digit errors, and counters past 2^64;
- `at`, `verify` with `valid_window`, and the timecode from
  `fromtimestamp`, `mktime` and `timegm`, with their range errors;
- `provisioning_uri` and `build_uri` with `quote` and `urlencode`, and
  `parse_uri` over `urlparse`, `parse_qsl` and `int()`;
- `strings_equal`, which compares codes after NFKC.

`mk.py` writes `data/`. It holds:
- an authenticator app's export of otpauth URIs (GitHub, AWS, Google,
  Steam, a YubiKey slot, a VPN) and 32 broken URIs;
- the users and login attempts of a site that checks TOTP codes, with
  codes typed late, early, twice, with spaces or dashes, or with
  fullwidth, superscript, circled or Arabic-Indic digits;
- hardware HOTP tokens and button presses, some far ahead of the server;
- new users to enrol, and `jobs.tsv`, the jobs.

Jobs run the classes on RFC 4226's and RFC 6238's test vectors and many
other inputs. Four programs are built on them: an authenticator that
lists every account's code and the seconds left, a login check that
allows one step of drift and refuses replays and locks an account after
three wrong codes, a token server that looks ten counters ahead and
resynchronises on two consecutive codes, and an enrolment that builds
each new user's URI and reads it back. `oracle.py` runs the jobs through
the package itself (`vendor/pyotp/`, unchanged) and wrote `expected.out`.
`gen_kara.py` writes the data section. `tables.py` writes the NFKD
table, and `mk.py` checks that the table covers every line of `data/`.

The quirks are kept. Some examples:
- a Google-style secret written in groups (`jbsw y3dp ehpk 3pxp`) is
  refused, since a space is not a base32 digit;
- `digits=0` gives `10000000000`;
- `valid_window=-1` never matches;
- a TOTP before 1970 truncates toward zero, so the 29 seconds before the
  epoch share step 0;
- a Steam object always uses 30-second steps, whatever `interval` says;
- a SHA-224 or SHA-384 TOTP writes a URI `parse_uri` refuses, and so
  does a Steam object, since it writes `digits=10`;
- a code typed in fullwidth digits verifies, and one in Arabic-Indic
  digits does not.

pyotp is under the MIT License (`LICENSE-pyotp.txt`).
