# Kata loops legacy auto-parallelised (M4b gate input)
Built for M4 plan item "the exact list of kata loops legacy auto-parallelised" (review/m4-plan-2026-10-24.md, the M4b gate). The data is in `m4-kata-auto-par-loops.json` beside this file.

## Method

- **Compiler and katas of one day.** Legacy at kara `7568ca9d6f`, the last commit before auto-par went off by default (`1de29e940`, 2026-10-06), with release LLVM and its own runtime archive, and kara-katas at `676e534e47`, the last kata commit before that. Today's katas cannot stand in: 357 of 929 bench programs no longer build on legacy (the D5 and `[v; n]` migrations).
- **What ran parallel is read off the binaries.** Every one of the 376 bench programs was built twice, `KARAC_AUTO_PAR=0` and on. 267 came out byte-identical. 92 link a parallel entry point: `karac_par_reduce` (a loop) in 82, `karac_par_run_auto` (a statement group) in 6, both in 4. The other 17 changed code but link neither, so nothing in them runs parallel.
- **Lines.** Each call to those entry points was mapped to its source line through DWARF (`KARAC_DEBUG_INFO=1`, `addr2line`). For loops, legacy's own `karac query concurrency` names the same loops: its `fanned_out` loops match the binaries exactly, program for program, and give the loop line where inlining hid it. `line_now` is the same loop in today's kata text (archive legacy's query over today's file).
- **Timings.** Each pair run three times interleaved on this 4-core cloud container, `KARAC_HASH_SEED=1`; medians. Every par binary printed the same bytes as its seq twin.

## Result

88 loops (87 reductions, 1 disjoint-write loop) and 10 statement groups, in 92 bench programs of 85 katas. Geometric mean speedup of par over seq: 2.03x. 77 programs run 1.5x faster or more, 1 between 1.1x and 1.5x, 4 within 10%, and 10 slower.

Legacy's statement groups were where it lost: compare_version (165) ran 318 times slower in par and count_nodes (222) 152 times slower, each a statement group spawned inside a hot function; add_two_numbers (2) ran at half speed.

## Programs

| kata | program | sites (function:line at 10-06, today) | seq s | par s | speedup | par cpu/wall |
|---|---|---|---|---|---|---|
| 1 | `brute_force.kara` | reduce `sum +` main:34 | 0.653 | 0.217 | 3.0x | 3.56 |
| 2 | `iterative.kara` | statements main:75 | 0.777 | 1.582 | 0.49x | 1.0 |
| 3 | `sliding_window.kara` | reduce `sum +` main:47 | 0.028 | 0.012 | 2.37x | 2.69 |
| 4 | `binary_search_partition.kara` | reduce `sum +` main:108 | 0.042 | 0.026 | 1.64x | 2.02 |
| 5 | `expand_around_center.kara` | reduce `sum +` main:67 | 0.618 | 0.199 | 3.11x | 3.5 |
| 5 | `expand_around_center_unchecked.kara` | reduce `sum +` main:69 | 0.623 | 0.225 | 2.77x | 3.29 |
| 6 | `row_buffers.kara` | reduce `sum +` main:89 | 0.233 | 0.062 | 3.74x | 3.35 |
| 7 | `reverse.kara` | reduce `sum +` main:64 | 1.274 | 0.339 | 3.75x | 3.91 |
| 8 | `atoi.kara` | reduce `sum +` main:91 | 0.1 | 0.047 | 2.1x | 2.91 |
| 9 | `palindrome.kara` | reduce `sum +` main:84 | 0.232 | 0.067 | 3.47x | 3.31 |
| 10 | `regex.kara` | reduce `sum +` main:83 | 0.309 | 0.077 | 4.03x | 3.77 |
| 11 | `container.kara` | reduce `sum +` main:71 | 0.334 | 0.073 | 4.56x | 3.33 |
| 12 | `greedy.kara` | reduce `sum +` main:72 | 0.491 | 0.146 | 3.37x | 3.41 |
| 13 | `greedy.kara` | reduce `sum +` main:93 | 1.095 | 0.331 | 3.31x | 3.48 |
| 14 | `vertical.kara` | reduce `sum +` main:132 | 0.128 | 0.051 | 2.54x | 3.36 |
| 15 | `three_sum.kara` | reduce `sum +` main:110 | 0.349 | 0.143 | 2.43x | 3.31 |
| 16 | `three_sum_closest.kara` | reduce `sum +` main:115 | 0.143 | 0.059 | 2.44x | 3.2 |
| 17 | `letter_combinations.kara` | reduce `sum +` main:103 | 0.108 | 0.049 | 2.21x | 3.08 |
| 18 | `four_sum.kara` | reduce `sum +` main:148 | 0.648 | 0.264 | 2.45x | 3.42 |
| 20 | `valid_parentheses.kara` | reduce `count +` main:129 | 1.405 | 0.359 | 3.91x | 3.43 |
| 21 | `iterative.kara` | reduce `total +` main:92 | 1.698 | 0.933 | 1.82x | 3.8 |
| 22 | `backtracking.kara` | reduce `total +` main:43 | 0.271 | 0.117 | 2.31x | 3.06 |
| 23 | `divide_and_conquer.kara` | reduce `total +` main:112 | 2.803 | 1.231 | 2.28x | 3.86 |
| 24 | `iterative.kara` | reduce `total +` main:76 | 0.876 | 0.489 | 1.79x | 3.7 |
| 25 | `iterative.kara` | reduce `total +` main:102 | 0.972 | 0.479 | 2.03x | 3.82 |
| 28 | `brute_force.kara` | reduce `sum +` main:55 | 0.591 | 0.708 | 0.83x | 3.73 |
| 28 | `kmp.kara` | reduce `sum +` main:68 | 0.405 | 0.116 | 3.5x | 3.59 |
| 28 | `kmp_unchecked.kara` | reduce `sum +` main:82 | 0.41 | 0.109 | 3.76x | 3.72 |
| 49 | `group_anagrams.kara` | reduce `sum +` main:70 | 0.047 | 0.023 | 2.01x | 2.37 |
| 52 | `nqueens2_bench.kara` | reduce `acc +` search:26 | 0.474 | 0.488 | 0.97x | 0.99 |
| 56 | `merge_intervals.kara` | reduce `sum +` main:99 | 0.122 | 0.044 | 2.76x | 3.29 |
| 57 | `insert_interval.kara` | reduce `sum +` main:134 | 0.057 | 0.032 | 1.81x | 2.2 |
| 57 | `insert_interval_cap.kara` | reduce `sum +` main:118 | 0.028 | 0.012 | 2.29x | 2.52 |
| 59 | `spiral_matrix_ii.kara` | reduce `total +` main:103 | 0.183 | 0.077 | 2.37x | 3.18 |
| 60 | `permutation_sequence.kara` | reduce `total +` main:86 | 0.051 | 0.015 | 3.37x | 3.33 |
| 60 | `permutation_sequence_nextperm.kara` | reduce `total +` main:84 | 0.161 | 0.037 | 4.3x | 3.57 |
| 65 | `valid.kara` | reduce `sum +` main:115 | 0.129 | 0.042 | 3.05x | 3.52 |
| 71 | `simplify.kara` | reduce `sum +` main:106 | 0.105 | 0.032 | 3.29x | 3.37 |
| 75 | `sort_colors.kara` | reduce `sum +` main:58 | 0.772 | 0.224 | 3.45x | 3.85 |
| 84 | `largest_rectangle.kara` | reduce `sum +` main:61 | 0.663 | 0.216 | 3.07x | 3.43 |
| 86 | `partition_list.kara` | reduce `sum +` main:86 | 0.709 | 0.351 | 2.02x | 3.86 |
| 87 | `scramble_string.kara` | reduce `sum +` main:120 | 0.652 | 0.169 | 3.86x | 3.9 |
| 125 | `valid_palindrome.kara` | reduce `sum +` main:59 | 1.161 | 0.348 | 3.33x | 3.68 |
| 128 | `longest_consecutive.kara` | reduce `sink +` main:71 | 0.321 | 0.09 | 3.54x | 3.44 |
| 137 | `single_number.kara` | reduce `res |` single_bitcount:48 | 0.022 | 0.029 | 0.75x | 2.83 |
| 141 | `linked_list_cycle.kara` | reduce `sink +` main:83 | 0.343 | 0.219 | 1.57x | 2.08 |
| 142 | `linked_list_cycle_ii.kara` | reduce `sink +` main:99 | 0.411 | 0.246 | 1.67x | 2.25 |
| 149 | `max_points.kara` | reduce `sum +` main:91 | 0.311 | 0.085 | 3.66x | 3.73 |
| 153 | `binary_search.kara` | reduce `sum +` main:48 | 0.043 | 0.024 | 1.77x | 2.3 |
| 159 | `longest_two_distinct.kara` | reduce `sink +` main:62 | 0.428 | 0.145 | 2.96x | 3.42 |
| 160 | `intersection.kara` | reduce `sink +` main:77; statements in the loop at main:77 | 0.386 | 0.11 | 3.5x | 3.58 |
| 161 | `one_edit.kara` | reduce `sink +` main:145 | 0.718 | 0.357 | 2.01x | 2.27 |
| 163 | `missing_ranges.kara` | reduce `sink +` main:48 | 1.375 | 0.376 | 3.66x | 3.68 |
| 165 | `compare_version.kara` | statements compare_version:30 | 0.68 | 216.543 | 0.003x | 1.34 |
| 171 | `column_number.kara` | reduce `sum +` main:61 | 0.714 | 0.185 | 3.85x | 3.46 |
| 172 | `trailing_zeroes.kara` | reduce `sink +` main:23 | 0.225 | 0.08 | 2.81x | 3.16 |
| 191 | `popcount.kara` | reduce `sink +` main:39 | 0.022 | 0.019 | 1.16x | 1.67 |
| 202 | `happy_number.kara` | reduce `sink +` main:35 | 0.4 | 0.119 | 3.35x | 3.34 |
| 204 | `count.kara` | reduce `primes collect` main:30 | 1.718 | 0.593 | 2.9x | 2.93 |
| 209 | `min_subarray.kara` | reduce `sink +` main:43 | 0.439 | 0.123 | 3.57x | 3.55 |
| 213 | `house_robber_ii.kara` | reduce `sink +` main:49; statements in the loop at main:49 | 0.345 | 0.161 | 2.15x | 3.34 |
| 217 | `contains_duplicate.kara` | reduce `sink +` main:42 | 11.139 | 3.093 | 3.6x | 3.79 |
| 220 | `contains_duplicate_iii.kara` | reduce `sink +` main:72 | 1.256 | 0.333 | 3.78x | 3.71 |
| 222 | `count_nodes.kara` | statements count_nodes:55 | 0.341 | 51.85 | 0.007x | 1.41 |
| 228 | `summary_ranges.kara` | reduce `sink +` main:48 | 1.294 | 0.373 | 3.46x | 3.78 |
| 233 | `digit_one.kara` | reduce `sink +` main:34 | 0.183 | 0.057 | 3.23x | 3.44 |
| 242 | `valid_anagram.kara` | reduce `sink +` main:75 | 0.154 | 0.074 | 2.07x | 3.01 |
| 248 | `strobogrammatic_count.kara` | reduce `n +` count_bounded:138; reduce `total +` count_in_range:173 | 0.281 | 0.393 | 0.71x | 1.06 |
| 270 | `bst_close.kara` | reduce `parts collect` main:120 | 0.495 | 0.145 | 3.43x | 3.44 |
| 273 | `spell.kara` | reduce `parts collect` main:133 | 0.792 | 0.234 | 3.38x | 3.71 |
| 276 | `paint_enum.kara` | reduce `parts collect` main:76 | 0.325 | 0.121 | 2.68x | 3.03 |
| 277 | `celebrity.kara` | reduce `parts collect` main:63 | 0.309 | 0.095 | 3.25x | 3.03 |
| 278 | `alien.kara` | reduce `parts collect` main:142; statements main:133 | 0.216 | 0.118 | 1.83x | 1.83 |
| 278 | `alien_seq.kara` | statements main:134 | 0.184 | 0.22 | 0.83x | 0.98 |
| 282 | `exprops.kara` | reduce `parts collect` main:103 | 0.894 | 0.256 | 3.49x | 3.7 |
| 282 | `exprops_pushstr.kara` | reduce `parts collect` main:107 | 0.922 | 0.279 | 3.3x | 3.63 |
| 286 | `wallsgates.kara` | reduce `parts collect` main:132 | 0.597 | 0.16 | 3.72x | 3.69 |
| 290 | `word_pattern.kara` | reduce `sink +` main:122 | 0.248 | 0.108 | 2.3x | 2.75 |
| 291 | `word_pattern_ii.kara` | reduce `sink +` main:89 | 0.366 | 0.164 | 2.23x | 3.25 |
| 302 | `blackpixels.kara` | statements min_area:97 | 0.463 | 0.22 | 2.11x | 2.04 |
| 306 | `additive.kara` | reduce `hits +` main:129 | 0.176 | 0.086 | 2.03x | 3.01 |
| 312 | `burst.kara` | reduce `best max` solve:29 | 0.489 | 0.866 | 0.56x | 0.98 |
| 313 | `ugly.kara` | reduce `best min` main:65 | 0.319 | 0.388 | 0.82x | 1.0 |
| 318 | `max_product.kara` | disjoint writes build_masks:45 | 1.149 | 1.464 | 0.79x | 0.99 |
| 332 | `reconstruct_itinerary.kara` | reduce `sink +` main:93 | 0.255 | 0.256 | 1.0x | 1.0 |
| 351 | `unlock_patterns.kara` | statements number_of_patterns:42 | 1.456 | 1.588 | 0.92x | 1.03 |
| 393 | `validate_utf8.kara` | reduce `count +` main:117 | 0.258 | 0.092 | 2.8x | 2.99 |
| 394 | `decode_string.kara` | reduce `sum +` main:70 | 0.327 | 0.104 | 3.14x | 3.33 |
| 722 | `remove_comments.kara` | reduce `sum +` main:112; reduce `total +` pass_len:77 | 0.156 | 0.065 | 2.4x | 2.98 |
| 726 | `number_of_atoms.kara` | reduce `checksum +` main:176 | 0.544 | 0.57 | 0.96x | 0.99 |
| 895 | `freq_stack.kara` | reduce `checksum +` run:88 | 0.041 | 0.019 | 2.14x | 2.85 |
| 3629 | `bfs_sieve.kara` | reduce `sum +` main:98; statements in the loop at main:98 | 7.674 | 3.789 | 2.03x | 3.28 |

## Written after the flip

Twelve bench programs written after 2026-10-06 link a parallel entry when today's text is built with archive legacy (`741fbf08c1`, `KARAC_AUTO_PAR=1 karac build --legacy`). They were never built parallel by default and have no legacy timing; they are listed for completeness, not for the gate.

- 417 `pacific_atlantic.kara`: statement group pacific_atlantic:47
- 419 `battleships.kara`: reduction count_battleships:11
- 458 `poor_pigs.kara`: reduction main:39; reduction main:54
- 461 `hamming_distance.kara`: reduction main:42
- 495 `teemo_attacking.kara`: reduction find_poisoned_duration:6
- 750 `count_corner_rectangles.kara`: reduction count_corner_rectangles:9
- 753 `crack_safe.kara`: reduction main:51
- 788 `rotated_digits.kara`: reduction rotated_digits:9
- 818 `racecar.kara`: reduction racecar:22
- 866 `prime_palindrome.kara`: reduction random_n:68
- 882 `reachable_nodes.kara`: reduction reachable_nodes:37
- 907 `sum_subarray_mins.kara`: disjoint write sum_subarray_mins:12

## Changed without a parallel entry

`bespoke/utf8-codepoints/bench/utf8_codepoints.kara`, `leetcode/1-100/45-jump-game-ii/bench/jump_game_ii.kara`, `leetcode/1-100/76-minimum-window-substring/bench/minimum_window_substring.kara`, `leetcode/101-200/126-word-ladder-ii/bench/word_ladder_ii.kara`, `leetcode/101-200/127-word-ladder/bench/word_ladder.kara`, `leetcode/101-200/132-palindrome-partitioning-ii/bench/min_cut.kara`, `leetcode/101-200/170-two-sum-iii-data-structure-design/bench/two_sum_iii.kara`, `leetcode/101-200/200-number-of-islands/bench/number_of_islands.kara`, `leetcode/201-300/205-isomorphic-strings/bench/isomorphic.kara`, `leetcode/201-300/208-implement-trie-prefix-tree/bench/trie.kara`, `leetcode/201-300/211-design-add-and-search-words-data-structure/bench/word_dictionary.kara`, `leetcode/201-300/231-power-of-two/bench/power_of_two.kara`, `leetcode/201-300/234-palindrome-linked-list/bench/palindrome_list.kara`, `leetcode/201-300/253-meeting-rooms-ii/bench/min_meeting_rooms.kara`, `leetcode/201-300/263-ugly-number/bench/ugly_sweep.kara`, `leetcode/201-300/283-move-zeroes/bench/movezero.kara`, `leetcode/201-300/286-walls-and-gates/bench/wallsgates_seq.kara`
