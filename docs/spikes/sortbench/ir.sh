#!/bin/bash
# Host-independent instruction counts via callgrind (no perf / PMU needed).
# Reports the SORT ALONE by subtracting the no-sort build of the same program.
#   usage: ir.sh            # karac, needs gen.py to have built ./bin
#          ir.sh drift      # driftsort, needs ./one built from one.rs
set -u
cd "$(dirname "$0")"
ir() { valgrind --tool=callgrind --callgrind-out-file=/dev/null "$@" 2>&1 \
       | grep 'I   refs' | tr -d ' ,' | sed 's/.*Irefs://'; }
for p in few_unique sawtooth random; do
  if [ "${1:-karac}" = drift ]; then
    s=$(ir ./one "$p" sort); b=$(ir ./one "$p" nosort)
  else
    s=$(ir "./bin/${p}_1_sort"); b=$(ir "./bin/${p}_1_cloneonly")
  fi
  d=$((s-b)); echo "$p sort=$((d/1000000)).$(( (d/100000)%10 ))M"
done
