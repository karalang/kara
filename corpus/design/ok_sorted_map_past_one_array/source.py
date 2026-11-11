# The Python mirror of source.kara (B-2026-11-10-39): expected.out is its output.
import bisect
def digest(a,x): return (a*1000003+x)%998244353
m={}
for i in range(20000): m[100000-3*i]=f"v{i}"
for i in range(5000): m[100001+2*i]=f"r{i}"
state=2092
def rnd():
    global state
    state=(state*1103515245+12345)&2147483647
    return state
for i in range(8000):
    rnd(); k=40000+(state//256)%60000; m[k]=f"s{k}"
out=[]
ks=sorted(m)
out.append(f"len {len(m)}")
out.append(f"first {ks[0]} {m[ks[0]]}")
out.append(f"last {ks[-1]} {m[ks[-1]]}")
d=0
for k in ks: d=digest(d,k+len(m[k]))
out.append(f"forward {len(ks)} {d}")
d=0;n=0
for k in reversed(ks):
    if n<50000: d=digest(d,k*7+len(m[k]))
    n+=1
out.append(f"backward {n} {d}")
hits=0
for q in range(3000):
    rnd(); k=30000+(state//256)%80000
    hits+= len(m[k]) if k in m else 1000
    i=bisect.bisect_right(ks,k)
    hits+= ks[i-1]%97 if i>0 else 5
    j=bisect.bisect_left(ks,k)
    hits+= ks[j]%89 if j<len(ks) else 7
out.append(f"probes {hits}")
d=0;n=0
for k in ks:
    if 41000<=k<77777: d=digest(d,k+len(m[k])); n+=1
out.append(f"range {n} {d}")
n=0
for k in ks:
    if 99997<=k<=100003: n=n*10+k%10
out.append(f"range_inclusive {n}")
gone=0
for k in range(30000,90000,5):
    if k in m: gone+=len(m.pop(k))
    else: gone+=1
out.append(f"removed {gone} len {len(m)}")
c={k:v+"!" for k,v in m.items() if k%4!=0}
cd=0
for k in sorted(c): cd=digest(cd,k+len(c[k]))
out.append(f"retained {len(c)} {cd}")
ks=sorted(m); ends=0
while len(ks)>40:
    k=ks.pop(0); ends=digest(ends,k+len(m.pop(k)))
    k=ks.pop(); ends=digest(ends,k*3+len(m.pop(k)))
out.append(f"ends {ends} len {len(m)}")
out.append(f"keys {len(ks)} {ks[0]} {ks[19]} {ks[-1]}")
m[-1]="neg"
out.append(f"first -1 neg")
s=set(90000-2*i for i in range(30000))
t=set(30000+3*i for i in range(9000))
u=s|t; x=s&t; y=s-t
out.append(f"sets {len(s)} {len(t)} {len(u)} {len(x)} {len(y)}")
sd=0
for e in sorted(x): sd=digest(sd,e)
back=0
for e in sorted(y,reverse=True): back=digest(back,e)
out.append(f"set digests {sd} {back}")
ir=0
for e in sorted(u):
    if 40000<=e<50000: ir+=e%7
out.append(f"set range {ir}")
dr=0
for e in sorted(s): dr=digest(dr,e)
out.append(f"drained {dr} 0 true")
print("\n".join(out))
