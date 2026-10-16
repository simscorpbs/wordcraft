import glob, re

W = 'C:/Users/Brenno/ptbr-rev/'
R = 'C:/Users/Brenno/wordcraft/'
D = R + 'crates/ui-egui/src/i18n/'


def keys(f):
    k = {}
    for l in open(f, encoding='utf-8'):
        l = l.rstrip('\n')
        if not l or l.startswith('#'):
            continue
        p = l.split('\t')
        if len(p) >= 3:
            k[(p[0], p[1])] = p[2]
    return k


pt = keys(D + 'pt-br.tsv')
allk = set()
for f in glob.glob(D + '*.tsv'):
    allk |= set(keys(f))
code = set()
pat = re.compile(r'(?:tl!|heading|\bt|tc|tr)\(\s*(?:[a-z_&.]+,\s*)?"((?:[^"\\]|\\.)*)"')
for f in glob.glob(R + 'crates/ui-egui/src/**/*.rs', recursive=True):
    if 'test' in f:
        continue
    s = open(f, encoding='utf-8').read()
    for m in pat.finditer(s):
        code.add(('', m.group(1)))
ptk = set(pt)
miss_union = sorted(allk - ptk)
miss_code = sorted(k for k in code - ptk - allk if re.search('[A-Za-z]{2}', k[1]))
open(W + 'faltam-no-ptbr.tsv', 'w', encoding='utf-8').write(''.join(c + '\t' + s + '\n' for c, s in miss_union))
open(W + 'faltam-em-todos.txt', 'w', encoding='utf-8').write(''.join(s + '\n' for c, s in miss_code))
print(len(pt), len(miss_union), len(miss_code))
