# pipeline/read_align

A sequencing alignment job, the shape of a genomics lab's nightly run. It
builds four chromosomes, plants variants in a donor genome for each,
simulates the sequencer's reads, writes the references as FASTA and the
reads as FASTQ, reads the files back and parses them, builds a k-mer index
of each reference, aligns every read, calls variants from the pileup, runs
five reports per chromosome, and registers the calls in one cohort
database, which it writes out as a VCF-style file.

- A chromosome is 5600 random bases with two additions: a 300-base mobile
  element that is the same in all four chromosomes, and a 120-base segment
  of its own copied a second time further on. Everything comes from a
  32-bit integer hash of a seed and a position.
- Each donor carries sixteen variants: ten SNPs, two deletions and two
  insertions of 1 to 3 bases at hashed positions at least 80 bases apart,
  plus a SNP and a 2-base deletion at fixed places in the mobile element.
  A planted indel is moved right until it cannot slide left, so the truth
  is in left-aligned form.
- The sequencer reads 60 bases at 8x coverage, from either strand, with
  about 0.8% substitution errors that get low base qualities. A read's
  FASTQ header carries the position it came from.
- The parsers check the FASTA header, the chromosome name, every base and
  the declared length, and each FASTQ record's shape, bases, qualities and
  header. A copy of a FASTQ file with one quality line cut short, and the
  chr2 FASTA read as chr4, show their errors.
- The index holds every 12-mer of a reference as `code * 8192 + position`,
  sorted. A read's five non-overlapping 12-mers vote for diagonals; up to
  three of the best, more than 8 apart, are aligned on each strand.
- The alignment is banded (8 either side of the diagonal), global in the
  read and free at both ends of the reference: +2 for a match, -3 for a
  mismatch, -5 for each gap base. The traceback prefers a match, then an
  insertion, then a deletion, so gaps come out left-aligned. MAPQ is 60
  for a single candidate and four times the gap to the second best
  otherwise, so reads in the copied segment get 0.
- The caller counts bases of quality 15 or more from reads of MAPQ 20 or
  more, and calls a SNP, deletion or insertion with at least 3 supporting
  reads and 30% of the depth (`1/1` from 80%).
- The reports: coverage (mean, uncovered bases, a depth histogram),
  mapping (MAPQ bands, reads placed at the position they came from, edits,
  gapped reads), the calls against the planted truth, the read with the
  most edits drawn against the reference, and composition (GC per kb, the
  longest run, distinct and repeated 12-mers, a checksum of the index).
- The cohort keys a call by the eight reference bases before it and its
  alleles. A new key gets the next id `kvN`; a key seen before points at
  the chromosome and position that brought it first. The two element
  variants are the ones every chromosome shares.

The output is honest about the caller: an insertion next to a run of the
same bases is sometimes seen as mismatches at the ends of reads, so a few
calls are false and a few planted indels are missed.

Everything is integer arithmetic, so the backends and the Python mirror
`read_align.py` agree by construction; the mirror wrote `expected.out`.

stmt-par: ten groups of statements that do not depend on each other, in
`run()` (`read_align.orig.kara` lines):
- L1276-L1279 four chromosome builds, each from its own seed;
- L1282-L1285 four donors;
- L1288-L1291 four sequencing runs;
- L1294-L1301 eight `fs.write` calls, each with the `encode_fasta` or
  `encode_fastq` call that builds its text;
- L1312-L1319 eight reads, each with `?`;
- L1330-L1337 eight parses. Each can fail with `?`, so a parallel run has
  to report the first failure in source order;
- L1352-L1355 four k-mer indexes;
- L1358-L1361 four alignment runs, about 800 reads each. This is where
  most of the time goes;
- L1364-L1367 four variant calls;
- L1370-L1389 twenty reports, five per chromosome, that only read what
  the earlier groups made.

The four `register` calls (L1397-L1408) are the case that must stay in
order: each takes `mut db`, ids go to calls in the order they arrive, and
a call seen before names the chromosome that registered it first, so
registering chromosomes out of order changes every later id and the
"first on" of every shared variant.

## Edits from read_align.orig.kara to read_align.kara

All author errors, and every diagnostic was right:
- `!` for negation in 15 places; `karac fix` replaced each with `not`.
- `const K` is a one-letter name, which is always Type-class, and the
  message says so and that no case change can fix it. It is `KMER` now,
  by hand, since a rename has no machine edit.
- `struct Entry` shadows the prelude's `Entry`. That is legal, but
  `karac run` and `karac build` print the warning on stderr, so the struct
  is `Known` now.

After those edits it matched the mirror on the first run under
`karac run`, the MIR interpreter, the LLVM backend, the ASan build and the
native binary, with nothing on stderr. No compiler gaps.

## Running

```bash
karac run read_align.kara
karac build --mir read_align.kara && ./read_align
python3 -I -B read_align.py > expected.out
```

About 22 s on the MIR interpreter (a debug build of `karac`) and 0.06 s
built. It writes its ten files to the working directory and removes them
at the end.
