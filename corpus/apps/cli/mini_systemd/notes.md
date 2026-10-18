# mini_systemd

systemd is the service manager on most Linux systems. The program ports
how it loads unit files and how `systemctl` runs jobs on them. It prints
what `mini_systemd.py` prints for the same `data/jobs.txt`.

The run writes into `data/`:
- vendor units in `usr/lib/systemd/system/`: seven targets, four
  services, the template `getty@.service` and `getty.target`;
- admin units in `etc/systemd/system/`: `webapp.service` with two
  drop-ins, `metrics-agent.service` with an `override.conf` that adds an
  ordering cycle, `backup.service`, which requires a mount service that
  fails, and `loop-a` / `loop-b`, which require each other in a cycle;
- `jobs.txt`, the `systemctl` commands to run.

**Loading.**
- The admin directory wins over the vendor one.
- An instance name like `getty@tty2.service` falls back to its template
  file. Specifiers `%n %N %p %i %%` expand in `Description=` and
  `ExecStart=`.
- Drop-ins `<unit>.d/*.conf` come from both directories. They apply in
  file name order, and the admin copy wins when both have the same name.
- In list settings (`Requires=`, `Wants=`, `After=`, `Before=`,
  `Conflicts=`, `WantedBy=`, `Environment=`), an empty assignment resets
  the list.
- Files in an admin `<unit>.wants/` directory add to `Wants=`.

**Commands.**
- `cat` prints the fragment and each drop-in under its path.
- `show` prints a fixed set of properties.
- `enable` writes the `.wants/` link as a file holding the target path,
  using `DefaultInstance=` for a template. A unit with no `[Install]`
  gets systemd's "no installation config" message.
- `disable` removes the links.
- `list-dependencies` draws the tree and expands only targets.
- `start` builds the transaction over `Requires=` and `Wants=`.
  - `Conflicts=` works in both directions and stops active units.
  - `After=` / `Before=` give the order.
  - An ordering cycle is broken by deleting its first job that is not
    required. When every job in it is required, the start is refused as
    "Transaction order is cyclic".
  - A unit whose `Requires=` dependency failed, and which is also ordered
    after it, fails with "Dependency failed".
  - A service whose `ExecStart=` has `--fail` exits with status 1.
- `stop` also stops every active unit that requires the stopped one,
  dependents first.
- `list-units` prints the aligned table.

`mk.py` holds the input files. `gen_kara.py` prints them as the Kāra
data functions in the program.

## Edits from mini_systemd.orig.kara to mini_systemd.kara

These were author errors, not compiler gaps:
- Pushing a bare `for` element into a Vec (nine places): iterating an
  owned temporary needs `.into_iter()`.
- Missing `mut` markers on the fresh bindings passed to the `mut ref`
  parameters of `dfs`, `cmd_start` and `cmd_stop`.

These are workarounds for gaps:
- `text.trim_end_matches("\n")` became the `chomp` helper, and the
  path and name searches scan bytes in `last_index_of`
  (B-2026-10-15-15: the string library has no reverse search or
  `strip_suffix`).
- `write_data` took the parent directory with
  `match .. { Some(i) => path[..i].to_string(), .. }`. Check then
  refused the later `fs.write(path, ..)` because the slice borrow from
  the match arm stayed live (B-2026-10-18-5). The index now comes from
  `unwrap_or(0)` and the slice is taken outside any match.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
