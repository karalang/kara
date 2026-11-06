#!/usr/bin/env python3
"""Writes Ryu's 125-bit power-of-five tables, POW5_INV_SPLIT and POW5_SPLIT,
as they end src/float_text.rs: `python3 scripts/gen-ryu-tables.py`.

POW5_SPLIT[i] is 5^i's leading 125 bits; POW5_INV_SPLIT[i] is
floor(2^(bitlen(5^i) - 1 + 125) / 5^i) + 1. Each is [low 64 bits, high]."""
B=125
def split(x):
    return (x & ((1<<64)-1), x >> 64)
inv=[];pos=[]
for i in range(342):
    p=5**i; l=p.bit_length(); j=l-1+B
    inv.append(split((1<<j)//p + 1))
for i in range(326):
    p=5**i; l=p.bit_length(); j=l-B
    pos.append(split(p>>j if j>=0 else p<<(-j)))
def emit(name,t):
    s=[f"const {name}: [[u64; 2]; {len(t)}] = ["]
    for a in range(0,len(t),2):
        s.append("    "+" ".join(f"[{lo:#018x}, {hi:#018x}]," for lo,hi in t[a:a+2]))
    s.append("];")
    return "\n".join(s)
print(emit("POW5_INV_SPLIT",inv)); print(); print(emit("POW5_SPLIT",pos))
