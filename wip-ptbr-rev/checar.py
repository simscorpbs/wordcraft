"""Confere um propostas.tsv contra o catálogo pt-br.tsv. Uso: python checar.py propostas.tsv"""
import re
import sys

CAT = 'C:/Users/Brenno/wordcraft/crates/ui-egui/src/i18n/pt-br.tsv'
CATS = ['nova', 'termo-word', 'gramatica', 'ingles', 'nao-cabe', 'sentido', 'consistencia', 'maiuscula']

cat = {}
for n, l in enumerate(open(CAT, encoding='utf-8'), 1):
    l = l.rstrip('\n')
    if not l or l.startswith('#'):
        continue
    p = l.split('\t')
    if len(p) >= 3:
        cat[(p[0], p[1])] = (n, p[2])

ph = re.compile(r'\{[a-z_0-9]+\}')
prob = 0
vistos = set()
for i, l in enumerate(open(sys.argv[1], encoding='utf-8-sig'), 1):
    l = l.rstrip('\r\n')
    if not l.strip():
        continue
    p = l.split('\t')
    if len(p) != 7:
        print(f'{i}: {len(p)} colunas, esperava 7'); prob += 1; continue
    linha, ctx, en, atual, prop, categ, motivo = p
    if i == 1 and linha == 'linha':
        continue
    if categ not in CATS:
        print(f'{i}: categoria desconhecida {categ!r}'); prob += 1
    if (ctx, en) in vistos:
        print(f'{i}: entrada repetida {en!r}'); prob += 1
    vistos.add((ctx, en))
    if (ctx, en) in cat:
        n, cur = cat[(ctx, en)]
        if linha and str(n) != linha:
            print(f'{i}: {en!r} está na linha {n}, não {linha}'); prob += 1
        if cur != atual:
            print(f'{i}: tradução atual de {en!r} não confere: {cur!r}'); prob += 1
        if cur == prop:
            print(f'{i}: proposta igual à atual'); prob += 1
    elif categ != 'nova':
        print(f'{i}: {en!r} não está no catálogo; use categoria nova'); prob += 1
    if sorted(ph.findall(en)) != sorted(ph.findall(prop)):
        print(f'{i}: placeholders diferem em {en!r}'); prob += 1
    if en.endswith('…') != prop.endswith('…'):
        print(f'{i}: reticências finais diferem em {en!r}'); prob += 1
    if not prop.strip():
        print(f'{i}: proposta vazia'); prob += 1
print(f'{prob} problemas')
sys.exit(1 if prob else 0)
