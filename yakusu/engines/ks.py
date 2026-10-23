"""Kirikiri KAG .ks scripts: read and write text in place.

KAG is line based:
  *label            labels
  @command a=b      commands
  ; comment         comments
  [iscript]...[endscript]   TJS code
  anything else     text, with inline [tags]

A physical newline inside text is NOT a line break in KAG; only [r] is.
So we join consecutive text lines into one unit (ending at a page break
[p], a command, a label, or a speaker name line), translate the unit, and
write the whole translation onto the unit's first line, blanking the rest.
Command lines never move, so backgrounds, sprites and voices stay in sync.

Speaker names: a line that is only 【name】 (or a name prefix on the
dialogue line) is treated as the speaker of the following text and written
back in the same bracket style.
"""
import re

NAME_LINE_RE = re.compile(r'^\s*([【［])([^】］]{1,24})([】］])\s*$')
NAME_PREFIX_RE = re.compile(r'^(【)([^】]{1,24})(】)')
TAG_RE = re.compile(r'\[[^\]]*\]')
JP_RE = re.compile(r'[぀-ヿ㐀-䶿一-鿿]')
PAGE_END_RE = re.compile(r'\[(p|pg|np|cm|er|s)\b[^\]]*\]\s*$')
STANDALONE_RE = re.compile(r'\[(link|sel|select|button|glink|choice)\b', re.I)
SCRIPT_BLOCK_START = re.compile(r'^\s*(@iscript|\[iscript\]|@macro|\[macro\b)', re.I)
SCRIPT_BLOCK_END = re.compile(r'^\s*(@endscript|\[endscript\]|@endmacro|\[endmacro\])', re.I)


def decode(data):
    """Return (text, encoding_label). Kirikiri accepts UTF-16LE BOM, UTF-8 BOM, or Shift-JIS."""
    if data[:2] == b'\xff\xfe':
        return data[2:].decode('utf-16-le'), 'utf-16'
    if data[:3] == b'\xef\xbb\xbf':
        return data[3:].decode('utf-8'), 'utf-8-sig'
    # No BOM: UTF-8 is very unlikely to decode by accident, so try it first.
    try:
        text = data.decode('utf-8')
        if any(ord(c) > 127 for c in text):
            return text, 'utf-8'
    except UnicodeDecodeError:
        pass
    try:
        return data.decode('cp932'), 'cp932'
    except UnicodeDecodeError:
        return data.decode('cp932', 'replace'), 'cp932'


def encode(text, label):
    if label == 'utf-16':
        return b'\xff\xfe' + text.encode('utf-16-le')
    if label == 'utf-8-sig':
        return b'\xef\xbb\xbf' + text.encode('utf-8')
    return text.encode(label, 'replace')


def _is_text_line(stripped):
    if not stripped or stripped[0] in '*@;':
        return False
    # Lines made only of tags (e.g. "[cm]", "[wait time=200]") are commands.
    if not TAG_RE.sub('', stripped).strip():
        return False
    return True


SPEAKER_TAG_RE = re.compile(r'^\[([^\s\]=\[]{1,16})(?:\s[^\]]*)?\]\s*$')
SPEAKER_CMD_RE = re.compile(r'^[@\[](?:nm|name|cn|talk|Talk|speaker|名前)\b[^\]]*?(?:t|text|name|n)\s*=\s*"?([^"\s\]]+)')


def _speaker_from_command(s):
    """Speaker named by a command line, e.g. [桜 voice=sak001] or @nm t="桜"."""
    m = SPEAKER_CMD_RE.match(s)
    if m and JP_RE.search(m.group(1)):
        return m.group(1)
    m = SPEAKER_TAG_RE.match(s)
    if m and JP_RE.search(m.group(1)):
        return m.group(1)
    return None


def parse(text):
    """Split a .ks file into translation units.

    Returns a list of dicts: {lines: [indices], message, name, name_line}.
    'message' is the joined text (tags as written); 'name_line' is the index
    of a separate 【name】 line, if any.
    """
    lines = text.split('\n')
    units = []
    cur = None
    pending_name = None  # (line index, name)
    in_script = False

    def close():
        nonlocal cur
        if cur:
            cur['message'] = ''.join(lines[i].rstrip('\r').strip() for i in cur['lines'])
            if JP_RE.search(TAG_RE.sub('', cur['message'])) or cur.get('name'):
                units.append(cur)
        cur = None

    for i, raw in enumerate(lines):
        s = raw.rstrip('\r').strip()
        if in_script:
            if SCRIPT_BLOCK_END.match(s):
                in_script = False
            continue
        if SCRIPT_BLOCK_START.match(s):
            close()
            in_script = True
            continue
        if not _is_text_line(s):
            close()
            spk = _speaker_from_command(s)
            if spk:
                pending_name = (None, spk, None, None)   # context only: the line itself is never rewritten
            continue
        m = NAME_LINE_RE.match(s)
        if m and JP_RE.search(m.group(2)) and not STANDALONE_RE.search(s):
            close()
            pending_name = (i, m.group(2), m.group(1), m.group(3))
            continue
        standalone = bool(STANDALONE_RE.search(s))
        if cur is None or standalone or cur.get('standalone'):
            close()
            cur = {'lines': [], 'name': None, 'name_line': None, 'standalone': standalone}
            if pending_name:
                cur['name_line'] = pending_name[0]
                cur['name'] = pending_name[1]
                if pending_name[2]:
                    cur['name_brackets'] = (pending_name[2], pending_name[3])
                pending_name = None
        cur['lines'].append(i)
        if PAGE_END_RE.search(s):
            close()
    close()
    return units


def export_entries(text):
    """msg-tool/VNTextPatch-style entries for a .ks file."""
    out = []
    for u in parse(text):
        e = {'message': u['message']}
        name = u.get('name')
        if not name:
            m = NAME_PREFIX_RE.match(u['message'])
            if m and JP_RE.search(m.group(2)):
                name = m.group(2)
        if name:
            e['name'] = name
        out.append(e)
    return out


def apply_entries(text, entries):
    """Write translated entries back. Entry order must match export_entries."""
    lines = text.split('\n')
    units = parse(text)
    if len(units) != len(entries):
        raise ValueError(f'unit count changed ({len(units)} vs {len(entries)})')
    for u, e in zip(units, entries):
        msg = e.get('message', u['message'])
        name = e.get('name')
        if u.get('name_line') is not None and name:
            o, c = u.get('name_brackets', ('【', '】'))
            nl = u['name_line']
            indent = re.match(r'^\s*', lines[nl]).group(0)
            lines[nl] = f'{indent}{o}{name}{c}' + ('\r' if lines[nl].endswith('\r') else '')
        elif name and NAME_PREFIX_RE.match(msg):
            msg = NAME_PREFIX_RE.sub(lambda m: f'{m.group(1)}{name}{m.group(3)}', msg, count=1)
        first = u['lines'][0]
        cr = '\r' if lines[first].endswith('\r') else ''
        indent = re.match(r'^\s*', lines[first]).group(0)
        lines[first] = indent + msg + cr
        for i in u['lines'][1:]:
            lines[i] = '\r' if lines[i].endswith('\r') else ''
    return '\n'.join(lines)
