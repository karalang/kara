# pipeline/grade_close

A school district's spring term close at four high schools, North, East,
South and West. It builds each school's students, its sections and its
gradebook, writes them as CSV, reads them back and parses them with the
district's checks, closes the term (curves, grades, lost credit, GPAs),
writes each school's report cards and GPAs, runs five reports per school,
and then pays the district's scholarship fund to the schools' nominees
until it runs out.

```
NHS students: 240 rows, 8,171 bytes, fnv 9bc5e43e
NHS sections: 46 rows, 2,853 bytes, fnv a0e29da3
NHS scores: 1,286 rows, 57,931 bytes, fnv 5dca8290
...
NHS_students.bad line 41: bad grade '13'
SHS_scores.bad line 201: bad h3 '11' (out of 10)
WHS_sections.csv line 2: section WHS100 is not at EHS
...
NHS grades: 1,286 section grades: A 60, A- 80, B+ 96, B 145, B- 108, C+ 109, C 145, C- 127, D+ 110, D 148, D- 64, F 91, I 3; 82 NC
...
NHS class of 2027: valedictorian Jordan Okafor 4.15, salutatorian Finley Moreau 4.09, third Harper Ueda 4.07
...
SHS scholarship: Casey Abara (3.93) $3,000.00
SHS scholarships: 5 nominated, $16,000.00 paid, 1 unpaid, $0.00 left in the fund
WHS scholarships: 5 nominated, $0.00 paid, 5 unpaid, $0.00 left in the fund
awards.log: 20 entries, 750 bytes, fnv 74d47333
```

## What it does

- **The sections**: in each grade, two sections each of English, math,
  science and history, the second honors in grades 9 and 10 and AP in 11
  and 12, and 14 elective sections (Spanish, French, studio art, physical
  education, computer science) open to every grade. Each has a teacher,
  a period from 1 to 7, a credit or half a credit, one of four sets of
  weights for homework, quizzes, tests and the final, zero to two dropped
  homeworks, and for some teachers a test curve up to a 75% mean.
- **The students**: 60 in each grade, with an id that carries the school
  and the grade, absences (some chronically absent), and the credits and
  GPA they bring from earlier terms.
- **The gradebook**: each student's four core courses (the stronger
  students try the advanced section first) and two electives, each in a
  free period of a section that is not full, and in each six homeworks
  out of 10, three quizzes out of 20, two tests out of 100 and a final
  out of 150. Some work is missing (`M`) or excused (`X`), and an excused
  final makes the course incomplete.
- **The parsers** check the header and final newline, the field count,
  student ids (six digits, the school's, once each, and in the grade the
  row says), names, grades, absences, credits with one decimal and GPAs
  with two; section ids (the school's, once each), courses, periods,
  levels, credits, weights that add up to 100, drops and curves; and that
  each gradebook row names a student and a section, a student once a
  section and once a period, and every score is `M`, `X` or no more than
  the item is out of. They report `file line N: what`. A copy of NHS's
  students with a thirteenth grader, a copy of SHS's gradebook with 11
  points on a homework out of 10, and WHS's sections read as EHS's show
  three of them.
- **The term close** curves each curving section's tests by the points
  that bring its mean up to the teacher's, grades every row (missing work
  counts zero, excused work is left out, the lowest homeworks are
  dropped, and a category with nothing left gives its weight to the
  others) to a percent and a letter, takes away the credit of a student
  absent more than 15 days, and computes each student's term GPA, the
  weighted one (half a point for honors and a point for AP on a C or
  better), the cumulative weighted GPA and the credits earned.
- **The report cards and GPAs** are written per school and read back.
- **The reports**: the grade distribution; sections curved, the lowest
  and highest averages and the three with the most Fs; the honor roll by
  grade (high honors at a 3.75 with no grade below B, honors at a 3.25
  with none below C) and the top three term GPAs; students with two or
  more Fs, over the absence limit, and seniors short of the 22 credits
  to graduate; and the senior class's top three.
- **The scholarships**: each school nominates its five best seniors with
  a cumulative weighted GPA of 3.50 or more, no F, no incomplete and the
  credits to graduate. The district's $60,000 fund pays each $5,000 at a
  4.00 and $3,000 otherwise, what it has left when that is less, and
  nothing once less than $1,000 is left. Every award goes in
  `awards.log`, which is written and read back.

Everything is integer arithmetic in hundredths of a percent, hundredths
of a grade point, tenths of a credit and cents, so the backends and the
Python mirror `grade_close.py` agree by construction; the mirror wrote
`expected.out`.

## How the port works

stmt-par: eight groups of statements that do not depend on each other,
in `run()` (`grade_close.kara` lines):
- L1119-L1122 four section builds and L1123-L1126 four student builds,
  then L1127-L1130 four gradebook builds, each over its own school's;
- L1133-L1144 twelve `fs.write` calls, each with the encoder that builds
  its text;
- L1161-L1172 twelve reads, each with `?`;
- L1189-L1192 four student parses and L1193-L1196 four section parses,
  then L1197-L1200 four gradebook parses, which read both. Each can fail
  with `?`, so a parallel run has to report the first failure in source
  order;
- L1217-L1220 four term closes, the hot group: each curves its school's
  sections and grades every row over data it alone owns;
- L1223-L1230 eight report-card and GPA writes and L1234-L1241 their
  reads;
- L1252-L1271 twenty reports, five per school, that only read what the
  earlier groups made.

The four `nominate` calls (L1287-L1298) are the case that must stay in
order: each takes `mut fund`, and a school's nominees are paid only from
what the schools before it left. In order, NHS and EHS are paid in
full, SHS's first four are paid and its fifth is not, and WHS gets
nothing; nominated in reverse, WHS and SHS are paid in full, EHS's fourth
nominee gets $1,000 and its fifth nothing, and NHS gets nothing.

## Edits from grade_close.orig.kara to grade_close.kara

`karac check` raised seven errors. `karac fix` applied four: three
section titles and course codes taken from `Str` constant arrays and
passed to `ref String` parameters, where the fix writes `.to_string()`,
and `let v = f[2 + t]`, which would move a `String` out of a `Vec`, where
the fix borrows it with `ref`. The other three were a compiler gap,
B-2026-10-25-6: `[Vec.new(), Vec.new(), Vec.new(), Vec.new()].to_vec()`
is refused with `expected 'Vec[?T74]', found 'Vec[?T75]'` for each
element after the first, because the elements' type variables are not
unified with each other. `grade_close.kara` spells it as the `Vec`
literal `Vec[Vec.new(), Vec.new(), Vec.new(), Vec.new()]` under the
binding's `Vec[Vec[i64]]` annotation, which the check accepts. After that
it matched the mirror on the first run under the MIR interpreter, the
LLVM backend, the ASan build and the native binary, with nothing on
stderr.

Kara fixed B-2026-10-25-6 in fd8b172278, so the literal is back to
`[Vec.new(), Vec.new(), Vec.new(), Vec.new()].to_vec()`. It still matches
the mirror on all four backends (`karac` at a6d57a78ee).

## Scope

One term at four schools, with no transfers in or out, no schedule
changes, no grade changes after the close, no summer school, no
standards-based grading, no attendance by course, no class rank beyond
the top three seniors, and no scholarship money from outside the
district.

## Running

```bash
karac run grade_close.kara
karac build --mir grade_close.kara && ./grade_close
python3 -I -B grade_close.py > expected.out
GRADE_REVERSE=1 python3 -I -B grade_close.py   # the schools nominate in reverse
```

About 7 s on the MIR interpreter (a debug build of `karac`), 3 s on the
LLVM backend and 0.02 s built. It writes its 23 files to the working
directory and removes them at the end.
