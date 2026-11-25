explain {}.a
Fixpoint: `a` is in the recursive group {a, b}
φ0 = ∅
φ1 = φ0 + panics, writes(Stdout)
a calls println, adding writes(Stdout)
φ2 = φ1 + allocates(Heap) = φ*
a calls b, adding allocates(Heap)
