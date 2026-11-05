# mini_kubesched

A pod scheduler in the shape of Kubernetes' kube-scheduler: nodes with
capacity, labels and taints; pods with resource requests, node selectors
and affinity, tolerations, anti-affinity and topology spread; a queue
ordered by priority; the filter and score plugins that place each pod;
preemption of lower-priority pods; and the day's events on a cluster
(drains, a NoExecute taint, a failed node, an autoscaled one). Each
manifest file is parsed in a task of its own.

```
> schedule
preempt on big-b2 for finance/quarter-close: evicts data/etl-2, data/etl-3, data/etl-4, data/etl-5
bind finance/quarter-close -> big-b2 (after preemption)
FailedScheduling data/train-small: 0/7 nodes are available: 1 node(s) had untolerated taint {node-role: control-plane}, 1 node(s) had untolerated taint {nvidia.com/gpu: present}, 5 node(s) didn't match Pod's node affinity/selector.
bound 1, pending 5
```

## Manifests

`data/*.nodes` and `data/*.pods` are loaded in file-name order; either
kind may hold nodes and pods. `#` starts a comment anywhere on a line. A
line that starts at column 0 opens a block; an indented line is a field of
the block above it. Tokens are separated by whitespace.

- `node NAME cpu=Q memory=Q pods=N` opens a node. All three are required
  (`node 'NAME' needs cpu, memory and pods`). Every node also carries the
  label `hostname=NAME`.
  - `label K=V ...`, `taint K=V:EFFECT` or `taint K:EFFECT`, and
    `unschedulable` (cordoned).
- `pod NAMESPACE/NAME [priority=N]` opens a pod; priority defaults to 0
  and may be negative.
  - `request cpu=Q memory=Q` (either may be left out: 0).
  - `label K=V ...`.
  - `select K=V ...`: the node must carry every label (nodeSelector).
  - `require KEY OP [V1,V2,...]`: required node affinity; every line must
    hold. OP is `In`, `NotIn`, `Exists`, `DoesNotExist`, `Gt`, `Lt`. `In`
    needs the key; `NotIn` holds when the key is missing; `Gt`/`Lt` take
    one integer and fail on a label that is not one (an optional `-` and
    digits).
  - `prefer W KEY OP [VALUES]`: preferred node affinity, weight 1..100.
  - `tolerate SPEC ...`: `*` tolerates everything; `K:EFFECT` and `K`
    tolerate any value of K (with that effect, or any); `K=V:EFFECT` and
    `K=V` tolerate that value.
  - `apart KEY SELECTOR`: required pod anti-affinity. No pod matching the
    selector may run on a node whose KEY label equals this node's.
  - `spread KEY MAXSKEW SELECTOR`: a topology spread constraint
    (DoNotSchedule), MAXSKEW at least 1.

A selector is `K=V[,K=V...]` and matches pods in the same namespace that
carry every label. Effects are `NoSchedule`, `PreferNoSchedule` and
`NoExecute`. CPU is cores (`2`, `0.5`, at most three decimals) or
millicores (`250m`); memory is bytes with an optional `Ki Mi Gi Ti`
(powers of 1024) or `k M G T` (powers of 1000) suffix.

The first error in a block is reported as `PATH:LINE: message` and drops
the whole block: `unknown directive`, `field outside a block`, `unknown
field`, `bad quantity`, `bad label`, `bad taint`, `bad effect`, `bad
toleration`, `bad operator`, `bad requirement`, `bad priority`, `bad
weight`, `bad skew`, `bad term`, `pod name '...' needs a namespace`.
After every file is read, a node or pod whose name was already taken is
reported as a duplicate at its own line and dropped. Pods are numbered in
the order they are admitted; that number breaks priority ties.

## Scheduling

`schedule` takes every pending pod, highest priority first and then in
admission order, and tries each once. A pod evicted while the cycle runs
waits for the next one.

**Filter.** Each node is checked in this order; the first plugin that
refuses gives the reasons:

1. a cordoned node: `node(s) were unschedulable`;
2. the first NoSchedule or NoExecute taint the pod does not tolerate:
   `node(s) had untolerated taint {KEY: VALUE}`;
3. `select` and `require`: `node(s) didn't match Pod's node
   affinity/selector`;
4. resources, every one that is short: `Too many pods`, `Insufficient
   cpu`, `Insufficient memory` (requests of the pods on the node plus the
   pod's own, against capacity);
5. spread: for each constraint the node needs the KEY label. Domains are
   the KEY values of the nodes that pass the pod's `select` and `require`;
   each counts the matching pods on such nodes. The pod fits when the
   node's domain count, plus one if the pod matches its own selector,
   minus the smallest domain count is at most MAXSKEW. Otherwise `node(s)
   didn't match pod topology spread constraints`;
6. the pod's `apart` terms: `node(s) didn't match pod anti-affinity
   rules`; then every running pod's `apart` terms against this pod (same
   KEY value, its selector matching this pod): `node(s) didn't satisfy
   existing pods anti-affinity rules`.

**Score.** Every node that passes gets, with integer division throughout,
and with a request of 0 counted as 100m CPU and 200Mi memory for every
pod involved:

- least allocated: per resource `(capacity - requested) * 100 /
  capacity`, the two averaged;
- balanced allocation: with each fraction as `requested * 10000 /
  capacity` (capped at 10000), `(20000 - |cpu - memory|) / 200`;
- node affinity: the weights of the `prefer` terms the node satisfies,
  scaled so the best node gets 100 (all 0 when none does);
- taint toleration: the PreferNoSchedule taints the pod does not tolerate,
  `100 - count * 100 / max` (100 when no node has one);
- spread: the matching pods in the node's domain summed over the
  constraints, `(max + min - count) * 100 / max` (100 when max is 0, and 0
  for a pod with no constraints).

The total is `least + balanced + 2 * affinity + 3 * taint + 2 * spread`.
The highest total wins, and the lowest node name breaks ties:
`bind POD -> NODE (score N)`.

**Preemption.** When no node fits, a node that refused only for resource
reasons is a candidate. All pods there with a lower priority are removed;
if the pod now passes every filter, they are added back one at a time,
highest priority first and then in admission order, keeping each one the
pod still fits beside. The rest are the victims. The node chosen has the
lowest highest-victim priority, then the lowest priority sum, then the
fewest victims, then the lowest name. The victims go back to pending:
`preempt on NODE for POD: evicts V1, V2` and `bind POD -> NODE (after
preemption)`.

Otherwise the pod stays pending with kube-scheduler's message: each
reason with the number of nodes that gave it, as `N reason`, sorted as
strings: `FailedScheduling POD: 0/N nodes are available: ....` The cycle
ends with `bound B, pending P`.

## Events

`data/events.txt` holds one event per line:

- `schedule`; `show nodes`; `show pods`; `show pending`.
- `apply PATH` reads another manifest file and admits its nodes and pods.
- `delete NAMESPACE/NAME`.
- `cordon NODE`, `uncordon NODE`; `drain NODE` cordons it and evicts
  every pod on it in name order (`evict POD from NODE (drain)`).
- `taint NODE SPEC`; a NoExecute taint evicts the pods that do not
  tolerate it. `untaint NODE KEY` removes every taint with that key.
- `fail NODE` removes the node; its pods go back to pending.

A mistake prints `error: data/events.txt:LINE: ...` (`no pod`, `no
node`, `bad event`, `cannot read PATH`). At the end the bindings are
written to `data/out/bindings.txt` as `POD NODE` lines in name order,
read back, and counted per node.

## Files

- `mk.py` writes `data/`; `oracle.py` is the reference in Python and
  writes `expected.out`; `gen_kara.py` embeds `data/` at the end of
  `mini_kubesched.kara`, which writes the files back before it starts.

## Gaps found

- B-2026-11-04-61: `m.get(k) == Some(v)` on a non-Copy value refuses
  with E0208 and no edit, and `karac fix` applies E0200's `.clone()`
  inside the `Some`, which leaves the E0208 standing. The program writes
  the no-copy form, `Some(ref v)`. Fixed in kara be4628d4bc: `karac fix`
  now writes `Some(ref v)` itself.
