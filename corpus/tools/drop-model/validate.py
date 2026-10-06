"""Check kmodel.py against the spec's own expectations: corpus/core pins and the drop matrix."""
import sys, glob, os, tomllib, traceback, collections as C
from pathlib import Path
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from kmodel import run_source, ModelError, Unsupported
K = str(Path(os.environ.get('KARA_ROOT', HERE.parents[2])) / 'corpus')
res = C.Counter(); bad = []
for area in ('core', 'drop-matrix'):
    for d in sorted(glob.glob(f'{K}/{area}/*')):
        meta = tomllib.load(open(d + '/meta.toml', 'rb'))
        exp = meta.get('expect', 'stdout'); want = open(d + '/expected.out').read() if os.path.exists(d + '/expected.out') else ''
        try:
            out, code, fl = run_source(open(d + '/source.kara').read()); got = ('run', out, code)
        except ModelError as e:
            got = ('error', str(e), None)
        except Unsupported as e:
            res[area + ':unsup'] += 1; bad.append((d, 'UNSUP ' + str(e))); continue
        except Exception as e:
            res[area + ':crash'] += 1; bad.append((d, 'CRASH ' + traceback.format_exc().splitlines()[-1])); continue
        if exp.startswith('error'):
            ok = got[0] == 'error'
        elif exp.startswith('panic'):
            ok = got[0] == 'run' and got[2] == int(exp.split(':')[1]) and got[1] == want
        else:
            ok = got[0] == 'run' and got[2] == 0 and got[1] == want
        res[area + (':ok' if ok else ':FAIL')] += 1
        if not ok: bad.append((d, f'want {exp} {want!r} got {got}'))
print(dict(res))
for d, m in bad[:40]: print(os.path.basename(d), '|', m[:300])
sys.exit(1 if any(k.endswith((':FAIL', ':crash')) for k in res) else 0)
