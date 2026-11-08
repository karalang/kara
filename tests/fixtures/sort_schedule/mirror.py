# The merge schedule every backend runs (src/mir/interp.rs `merge_order`):
# at most SMALL_SORT = 20 elements, one insertion sort; longer, runs of
# four by a network, then merges from both ends at once, redone
# from the front when the two ends cross. Prints what
# tests/mir_sort_network_merge.rs expects of its comparators that are not
# an order (B-2026-11-06-34), and, on stderr, how many merges crossed.
import sys
MB=1024; RUN=4; SMALL_SORT=20
cross=[0]
def sort4(v,cmp):
    c1=cmp(v[1],v[0]); c2=cmp(v[3],v[2])
    a,b=(1,0) if c1 else (0,1); c,d=(3,2) if c2 else (2,3)
    c3=cmp(v[c],v[a]); c4=cmp(v[d],v[b])
    mn=c if c3 else a; mx=b if c4 else d
    l=a if c3 else (c if c4 else b); r=d if c4 else (b if c3 else c)
    c5=cmp(v[r],v[l]); lo,hi=(r,l) if c5 else (l,r)
    return [v[mn],v[lo],v[hi],v[mx]]
def small(o,f,t,cmp):
    lo=f
    while lo+RUN<=t: o[lo:lo+4]=sort4(o[lo:lo+4],cmp); lo+=4
    ins(o,lo,t,cmp)
def ins(o,lo,t,cmp):
    for k in range(lo+1,t):
        x=o[k]; j=k
        while j>lo and cmp(x,o[j-1]): o[j]=o[j-1]; j-=1
        o[j]=x
def mpass(o,f,t,w,cmp):
    out=[None]*(t-f); lo=f
    while lo<t:
        mid=min(lo+w,t); hi=min(lo+2*w,t)
        i,j,k=lo,mid,lo; ie,je,ke=mid,hi,hi
        for _ in range(min(mid-lo,hi-mid)):
            if cmp(o[j],o[i]): out[k-f]=o[j]; j+=1
            else: out[k-f]=o[i]; i+=1
            k+=1; ke-=1
            if cmp(o[je-1],o[ie-1]): out[ke-f]=o[ie-1]; ie-=1
            else: out[ke-f]=o[je-1]; je-=1
        if i>ie or j>je: cross[0]+=1; i,j,k,ie,je=lo,mid,lo,mid,hi
        while i<ie and j<je:
            if cmp(o[j],o[i]): out[k-f]=o[j]; j+=1
            else: out[k-f]=o[i]; i+=1
            k+=1
        for x in o[i:ie]+o[j:je]: out[k-f]=x; k+=1
        lo=hi
    return out
def morder(n,cmp):
    o=list(range(n))
    if n<=SMALL_SORT: ins(o,0,n,cmp); return o
    if n>MB:
        for b in range(0,n,MB):
            e=min(b+MB,n); small(o,b,e,cmp); w=RUN
            while w<MB: o[b:e]=mpass(o,b,e,w,cmp); w*=2
        w=MB
    else: small(o,0,n,cmp); w=RUN
    while w<n: o=mpass(o,0,n,w,cmp); w*=2
    return o
def h(v):
    x=0
    for k,i in v: x=(x*31+k*1000+i)%1000000007
    return x
for n in [4,5,9,16,37,100,1025,3000]:
    v=[((i*7919)%101,i) for i in range(n)]
    calls=[0]
    def ca(a,b):
        calls[0]+=1; return calls[0]%2==0
    cross[0]=0
    o=morder(n,lambda a,b: ca(v[a],v[b])); a=[v[i] for i in o]; ac=cross[0]
    s=[n]
    def cb(a,b):
        s[0]=(s[0]*1103515245+12345)&2147483647; return (s[0]//65536)%3==0
    o=morder(n,lambda a,b: cb(v[a],v[b])); bb=[v[i] for i in o]
    o=morder(n,lambda a,b: (v[a][1]*7+v[b][1])%5<2); c=[v[i] for i in o]
    print(n,calls[0],'true true true',h(a),h(bb),h(c),'crossings',cross[0],'alt',ac, file=sys.stderr)
    print(n,calls[0],'true true true',h(a),h(bb),h(c))
