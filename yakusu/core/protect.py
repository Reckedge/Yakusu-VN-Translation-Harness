"""Keep engine control codes intact through translation.

Exported scripts still contain inline codes: Kirikiri tags like [l] or
<link target="*a">, escape sequences, %-variables, colour codes and so on.
Models happily translate or drop those, which crashes or garbles the game.
Before sending a line out we swap every code for a numbered placeholder
(⟦1⟧, ⟦2⟧ ...), then put the originals back and check that every
placeholder came back exactly once.

Presentation details are handled here too:
- Line breaks between plain text are layout: removed, then re-wrapped for
  English later. A break right after a code (e.g. [l][r], or between two
  choice links) is structure: it stays attached to that code.
- Furigana/ruby markup is dropped (it makes no sense over English).
- A leading 【name】 and 「」/『』 around dialogue are lifted off and put back
  at build time, in English.
"""
import functools
import re
from dataclasses import dataclass, field

PH_OPEN, PH_CLOSE = '\u27e6', '\u27e7'  # ⟦ ⟧
PH_RE = re.compile(PH_OPEN + r'(\d+)' + PH_CLOSE)

CODE_PATTERNS = [
    r'\[[A-Za-z_@][^\]\n]{0,160}\]',          # KAG tags: [l] [p] [link target=*a] [font ...]
    r'<[A-Za-z/][^<>\n]{0,160}>',             # msg-tool XML-ish tags: <l> <link target="*a"> </b>
    r'\\[A-Za-z](?:\[[^\]\n]{0,20}\]|\{[^}\n]{0,20}\}|<[^>\n]{0,20}>)?',  # \c[2] style escapes
    r'%[A-Za-z][A-Za-z0-9_]{0,15};?',          # %name; variables
    r'\$[A-Za-z_][A-Za-z0-9_]{0,30}',           # $var
    r'\{[\x20-\x7E]{1,40}\}',                   # {var}
    r'@[A-Za-z][A-Za-z0-9_]{0,20}',             # @commands inline
    r'#[0-9A-Fa-f]{6}(?:[0-9A-Fa-f]{2})?\b',    # colour codes
    r'&(?:[a-z]{2,6}|#\d{1,6});',               # XML entities
]
CODE_RE = re.compile('|'.join('(?:%s)' % p for p in CODE_PATTERNS))

# Furigana markup, removed for English.
RUBY_RE = re.compile(r'\[ruby\b[^\]]*\]|<ruby\b[^>]*>|</ruby>|\[/ruby\]|<rt>[^<]*</rt>|</?rb>|</?rt>', re.I)
# Aozora-style ruby: ｜漢字《かんじ》 -> 漢字
AOZORA_RUBY_RE = re.compile(r'[｜|]?([^｜|《》\s]+)《[^》]{1,20}》')

NAME_PREFIX_RE = re.compile(r'^(【|［)([^】］]{1,24})(】|］)')
QUOTE_PAIRS = {'「': '」', '『': '』'}
JP_RE = re.compile(r'[\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff]')


@dataclass
class Protected:
    text: str
    codes: list = field(default_factory=list)
    open_quote: str = ''
    close_quote: str = ''
    indent: str = ''
    name_prefix: tuple = ()        # (open, jp name, close)
    lead: str = ''                 # placeholders before the opening quote
    trail: str = ''                # placeholders after the closing quote


def has_japanese(s):
    return bool(JP_RE.search(PH_RE.sub('', s or '')))


def protect(message, line_break='\n'):
    """Prepare a source message for the model. Cached: the same line is
    protected many times over a run (translate, review, fit, build, preview).
    Treat the returned object as read-only."""
    return _protect(message or '', line_break)


@functools.lru_cache(maxsize=250000)
def _protect(message, line_break):
    s = message or ''
    p = Protected(text='')

    m = re.match(r'^[\u3000 \t]+', s)
    if m:
        p.indent = m.group(0)
        s = s[m.end():]

    s = RUBY_RE.sub('', s)
    s = AOZORA_RUBY_RE.sub(r'\1', s)

    breaks = {line_break, '\n', '\r\n'}
    br_alt = '|'.join(re.escape(b) for b in sorted(breaks, key=len, reverse=True))
    token_re = re.compile(f'({br_alt})|({CODE_RE.pattern})')
    codes, out = [], []
    pos = 0
    prev_code = False
    for mo in token_re.finditer(s):
        before = s[pos:mo.start()]
        if before:
            out.append(before)
            prev_code = False
        tok = mo.group(0)
        if mo.group(1) is not None:          # a line break
            rest = s[mo.end():]
            at_edge = not ''.join(out).strip() or not CODE_RE.sub('', rest).replace(line_break, '').strip()
            if prev_code and codes:
                codes[-1] += tok               # structural: keep with the code
            elif at_edge:
                # A break at the very start or end of a message separates it from
                # text around it in the script: keep it as its own code.
                codes.append(tok)
                out.append(f'{PH_OPEN}{len(codes)}{PH_CLOSE}')
                prev_code = True
            # else: layout break inside the text, dropped (we re-wrap)
        else:
            codes.append(tok)
            out.append(f'{PH_OPEN}{len(codes)}{PH_CLOSE}')
            prev_code = True
        pos = mo.end()
    out.append(s[pos:])
    s = ''.join(out).replace('\r', '')

    # Leading placeholders (e.g. a clear-screen code) stay outside the quotes.
    lead = re.match(r'^(?:' + PH_RE.pattern + r')+', s)
    if lead:
        p.lead = lead.group(0)
        s = s[lead.end():]
    nm = NAME_PREFIX_RE.match(s)
    if nm and JP_RE.search(nm.group(2)):
        p.name_prefix = (nm.group(1), nm.group(2), nm.group(3))
        s = s[nm.end():]
    trail = re.search(r'(?:' + PH_RE.pattern + r')+$', s)
    if trail:
        p.trail = trail.group(0)
        s = s[:trail.start()]
    st = s.strip()
    if len(st) >= 2 and st[0] in QUOTE_PAIRS and st[-1] == QUOTE_PAIRS[st[0]] and st.count(st[0]) == 1:
        p.open_quote, p.close_quote = st[0], st[-1]
        st = st[1:-1]
    p.text = st
    p.codes = codes
    return p


def check(p, translated):
    """Problems with a translation of p, as a list of strings (empty = fine)."""
    problems = []
    t = (translated or '').strip()
    if not t and p.text.strip():
        problems.append('empty')
    in_body = set(int(x) for x in PH_RE.findall(p.text))
    found = [int(x) for x in PH_RE.findall(t)]
    if sorted(found) != sorted(in_body):
        missing = sorted(in_body - set(found))
        if missing:
            problems.append(f'missing codes {missing}')
        extra = sorted({n for n in found if n not in in_body or found.count(n) > 1})
        if extra:
            problems.append(f'unexpected codes {extra}')
    if has_japanese(t):
        problems.append('still has Japanese')
    return problems


def restore(p, translated, quotes=('"', '"'), name=None):
    """Rebuild a full message from the translated body.

    quotes: (open, close) to use for dialogue that had 「」, or None to keep 「」.
    name:   English for a 【name】 prefix (falls back to the Japanese).
    """
    body = (translated or '').strip()
    in_body = [int(x) for x in PH_RE.findall(p.text)]
    found = set(int(x) for x in PH_RE.findall(body))
    for i in in_body:                       # re-append codes the model dropped
        if i not in found:
            body += f'{PH_OPEN}{i}{PH_CLOSE}'
    if p.open_quote:
        o, c = (p.open_quote, p.close_quote) if quotes is None else quotes
        body = f'{o}{body}{c}'
    prefix = ''
    if p.name_prefix:
        prefix = f'{p.name_prefix[0]}{name or p.name_prefix[1]}{p.name_prefix[2]}'
    full = p.lead + prefix + body + p.trail

    used = set()

    def back(mo):
        i = int(mo.group(1))
        if not 1 <= i <= len(p.codes) or i in used:
            return ''  # invented or repeated placeholder: drop it
        used.add(i)
        return p.codes[i - 1]
    return PH_RE.sub(back, full)


def strip_codes(s):
    return CODE_RE.sub('', PH_RE.sub('', s or ''))
