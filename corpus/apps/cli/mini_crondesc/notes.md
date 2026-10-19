# mini_crondesc

cron_descriptor 2.2.1 as Python 3.13 runs it. cron_descriptor turns a
cron expression into a sentence, such as `0 9-17 * * 1-5` into "At 0
minutes past the hour, between 09:00 AM and 05:59 PM, Monday through
Friday". Scheduler dashboards and admin pages use it to show what a
schedule means. The port covers:
- The expression parser. It takes the five-field crontab form, six fields
  with seconds or with a year, and seven-field Quartz expressions. It
  normalises them as the library does: `?` becomes `*`, `0/n` and `1/n`
  become `*/n`, day and month names become numbers, the week can start
  at 0 or 1, and a single hour becomes a range when minutes or seconds
  step.
- The description of each field. That covers lists, ranges, steps over
  either, `L`, `LW`, `nW`, `L-n`, `n#k`, `nL`, weekly steps over days,
  12-hour and 24-hour times, the three casings and the verbose form.
- The gettext catalogs the phrases come from. The port reads the 32 `.mo`
  files the package ships, with gettext's errors and the library's
  fallbacks: a locale with no catalog gets `en_US`, and a phrase missing
  from a catalog comes back empty rather than in English.
- `ExpressionValidator`, a separate class with its own rules and error
  messages.
- The `cron_descriptor` command line. Its argparse parser is ported for
  its one positional argument and four options, including prefix
  matching, stacked flags, `--` and argparse's error messages.

The run writes `data/` and then works through the 2537 jobs in
`data/jobs.jsonl`:
- 2079 descriptions. They come from system crontabs, Kubernetes
  CronJobs, Airflow, GitHub Actions, Quartz and AWS EventBridge, plus odd
  and broken expressions and 260 random ones. The option sets vary.
  Sixteen schedules are described in every shipped language and in a
  few unknown locales. A team's own catalogs are read through
  `locale_location`: a British one, a half-finished pirate one, one with
  no header, and three broken files. Each field is also described on its
  own, options are passed as keyword arguments (some unknown), and the
  system locale is set for an `Options()` that is not given one.
- 400 validations.
- 51 command lines, each printing the description, the help text, or the
  usage error and exit status.
- Seven runs over three files: a crontab with environment lines and `@`
  macros, a list of Kubernetes CronJob schedules, and Quartz properties.
  Each one is described line by line, in a chosen language.

`oracle.py` runs the same jobs through the package in `cron_descriptor/`,
which is the 2.2.1 wheel unchanged, and wrote `expected.out`. For the
command line it sets the system locale through `locale.getlocale`, and it
prints the last line of a traceback in place of the whole traceback.

cron_descriptor's quirks are kept. Some examples:
- Month and day names come from Python's `calendar` in the C locale. So
  they are English in every language: "Um 04:30, nur am Sunday".
- The shipped `en_US` catalog has no entry for the error message. So a
  description that fails raises `FormatError` with an empty message.
- `CasingTypeEnum.Title` uses `str.title()`, which gives "06:00 Am".
- An interval's phrase is formatted twice. So describing the minutes of
  `*/{1} * * * *` on their own raises `IndexError`.
- A range with no start is still a range: `-5 * * * *` reads as
  "Minutes  through 5 past the hour".
- The validator refuses a lone `jan` or `JAN-mar` but accepts
  `jan,FEB`, and it reports an unknown day in a list as a bare
  `KeyError`.
- A catalog of two bytes is not an `OSError`. So it does not fall back
  to `en_US`, and the `struct.error` reaches the caller.

The port matched `expected.out` on its first run. It needed no
workarounds and added no new ledger rows.

## License

cron_descriptor is under the MIT license in `LICENSE-MIT`:

    Copyright (c) 2016 Adam Schubert

That covers the package in `cron_descriptor/` and its catalogs, which
`mk.py` copies into `data/locale/` unchanged.
