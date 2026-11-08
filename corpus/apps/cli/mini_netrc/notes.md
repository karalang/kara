# mini_netrc

Python's `netrc` module: the reader of the `.netrc` files that ftp, curl,
git and pip take their credentials from. Machines with a login, account
and password, a `default` entry, `macdef` macros, and the module's own
lexer, with its quoting and its comment rules, quirks included.

The program writes its own data (`data/*.netrc`, every credential in them
fake, and `data/script.txt`), plays the script and prints a report.
`oracle.py` plays the same script against the vendored `netrc.py` from
CPython 3.13 (`vendor/`, PSF licence) and is the reference; `mk.py`
writes the data and `gen_kara.py` embeds it in the program.

## The script

- Each line is split into words at runs of spaces. Blank lines and lines
  whose first word starts with `#` are skipped. Every other line is echoed
  as `> ` followed by its words joined by one space, then runs; what it
  prints follows, each line indented by two spaces.
- A problem is reported as `  error: script.txt:<line>: <message>`. A
  parse error is `NetrcParseError: <msg> (<path>, line <n>)`; the rest are
  the port's own and given here in full.
- `parse NAME` reads `data/NAME.netrc` (`no file data/NAME.netrc` when
  there is none) and prints
  `parsed data/NAME.netrc: <h> hosts, <m> macros`. A failed parse leaves
  NAME unparsed, even when an earlier one succeeded.
- `dump NAME` prints the module's `repr`, a line at a time, each after
  `| `. `auth NAME HOST` prints the `repr` of `authenticators(HOST)`: the
  `(login, account, password)` of HOST, else of `default`, else `None`.
  `macro NAME MACRO` prints the `repr` of the macro's list of lines
  (`no macro <MACRO>` when there is none). An unparsed NAME is
  `<NAME> is not parsed`.
- Wrong word counts are `usage: parse NAME`, `usage: dump NAME`,
  `usage: auth NAME HOST` and `usage: macro NAME MACRO`; any other command
  is `unknown command <word>`. The report ends with `== end` and
  `  <n> files parsed`.

## The lexer

The file is read as UTF-8 text, with `\r\n` and a lone `\r` read as
`\n`. A line number starts at 1 and counts the newlines the lexer reads
one character at a time; the lines a comment or a macro skips are read
whole and are not counted.

- A token starts at the first character that is not a space, tab, `\r`
  or newline, and is `None` at the end of the file.
- A token that starts with `"` runs to the next `"`, whitespace included;
  a backslash takes the next character as it is. At the end of the file
  the token is what was read.
- Any other token runs to the next whitespace character, which is read
  and dropped; a backslash takes the next character as it is.
- A token can be pushed back, and the next read returns it first.

## The parser

- At the top, a token starting with `#` is a comment. When it is the
  single character `#` and no newline was read since the top-level read
  began, the rest of its line is skipped. Otherwise the comment ends with
  the token itself, and the next word is read as a token.
- `machine NAME` and `default` start an entry; an empty or missing NAME is
  `missing 'machine' name`. `macdef NAME` reads the rest of the current
  line and the lines after it as the macro's lines, up to an empty line
  (`Macro definition missing null line terminator.` at the end of the
  file); a second `macdef` of the same NAME replaces its lines. Any other
  token is `bad toplevel token <repr>`.
- In an entry, a token starting with `#` is a comment, whose rest of line
  is skipped when no newline was read since the entry's read began.
  `login` or `user`, `account` and `password` take the next token (`''`
  at the end of the file). `machine`, `default`, `macdef` or the end of
  the file end the entry and are pushed back. Any other token is
  `bad follower token <repr>`.
- An entry is stored under its name with its login, account and password,
  each `''` when not given; a name seen again keeps its first place in the
  order and takes the new values.
- `repr` writes, for each entry, `machine NAME`, a tab and
  `login LOGIN`, a tab and `account ACCOUNT` when the account is not
  empty, and a tab and `password PASSWORD`, each on its own line; then for
  each macro `macdef NAME`, its lines as read and an empty line.

## Scope

- The `~/.netrc` ownership and permission check runs only when no file
  name is given, so never here. Encodings other than UTF-8 are out of
  scope.
- No string a `repr` shows holds both kinds of quote.

## Running

```bash
python3 -s -P -B mk.py            # writes data/
python3 -I -B gen_kara.py         # embeds data/ in the program
python3 -I -B oracle.py > expected.out
karac run mini_netrc.kara
karac build mini_netrc.kara && ./mini_netrc
```
