"""Make English fit the game's text box.

Most VN engines don't word-wrap: they break wherever the script says, or
wrap at a fixed number of character cells (and will happily cut "beautiful"
into "beauti / ful"). So we:

1. Measure the box from the original Japanese script. Japanese characters
   are two cells wide, ASCII one. The longest lines and the most lines per
   message in the originals tell us the box size.
2. Word-wrap the English to that width, inserting the engine's line break.
3. Report messages that still need more lines than the box has, so the
   pipeline can ask the model for a shorter version.
4. Replace characters the engine's text encoding can't show (Shift-JIS has
   no em dash or accented letters).
"""
import re
import unicodedata

from .protect import CODE_RE, PH_RE


def cell_width(ch):
    if ch in '​﻿':
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ('F', 'W') else 1


def text_cells(s):
    s = CODE_RE.sub('', PH_RE.sub('', s or ''))
    return sum(cell_width(c) for c in s)


def _percentile(values, pct):
    if not values:
        return 0
    v = sorted(values)
    k = min(len(v) - 1, max(0, int(round((pct / 100) * (len(v) - 1)))))
    return v[k]


def measure_box(messages, line_break='\n'):
    """Guess the text box from original messages.

    Returns {mode, width, lines, sample}: mode 'explicit' when the script
    breaks lines itself (we must too), 'auto' when the engine wraps.
    """
    msgs = [m for m in messages if m and m.strip()]
    if not msgs:
        return {'mode': 'auto', 'width': 56, 'lines': 3, 'sample': 0}
    with_breaks = [m for m in msgs if line_break in m]
    explicit = len(with_breaks) >= max(5, 0.15 * len(msgs))
    if explicit:
        widths, counts = [], []
        for m in msgs:
            parts = m.split(line_break)
            counts.append(len(parts))
            widths.extend(text_cells(p) for p in parts)
        width = _percentile(widths, 99)
        lines = max(2, _percentile(counts, 99))
    else:
        # Engine wraps on its own; estimate a typical box from long messages.
        lens = [text_cells(m) for m in msgs]
        long_msgs = [x for x in lens if x >= 40] or lens
        width = min(_percentile(long_msgs, 50), 64) if long_msgs else 56
        width = max(width, 40)
        lines = 3
    width = int(max(24, min(width, 120)))
    return {'mode': 'explicit' if explicit else 'auto', 'width': width, 'lines': int(min(lines, 8)), 'sample': len(msgs)}


TOKEN_RE = re.compile(r'(\s+)')


def wrap(text, width, line_break='\n'):
    """Word-wrap by cells, keeping existing hard breaks. Returns (text, line count).

    Codes count as zero width and are never split. A break that's already in
    the text (e.g. one kept with a [l][r] code) starts a new line.
    """
    if width <= 0 or not text:
        return text, 1 if text else 0
    parts = text.split(line_break) if line_break else [text]
    out, total = [], 0
    for k, part in enumerate(parts):
        if not part.strip():
            out.append(part)
            if k < len(parts) - 1:
                total += 1
            continue
        w, n = _wrap_para(part, width, line_break)
        out.append(w)
        total += n
    return line_break.join(out), max(total, 1)


def _wrap_para(text, width, line_break):
    words = [w for w in TOKEN_RE.split(text) if w and not w.isspace()]
    lines, cur, cur_w = [], '', 0
    for w in words:
        ww = text_cells(w)
        if not cur:
            while ww > width:  # a single word longer than the box: hard break
                cut = _cut_at(w, width)
                lines.append(w[:cut])
                w = w[cut:]
                ww = text_cells(w)
            cur, cur_w = w, ww
        elif ww == 0 or cur_w + 1 + ww <= width:
            cur += ('' if ww == 0 else ' ') + w
            cur_w += (0 if ww == 0 else 1 + ww)
        else:
            lines.append(cur)
            cur, cur_w = w, ww
    if cur:
        lines.append(cur)
    return line_break.join(lines), len(lines)


def _cut_at(word, width):
    total = 0
    for i, ch in enumerate(word):
        total += cell_width(ch)
        if total > width:
            return max(1, i)
    return len(word)


# Characters that Shift-JIS (cp932) can't encode, with safe stand-ins.
SJIS_REPLACE = {
    '—': '―',   # em dash -> horizontal bar (exists in SJIS)
    '–': '-',
    '‒': '-',
    ' ': ' ',
    ' ': ' ',
    ' ': ' ',
    '…': '…',   # … is fine
}

STRAIGHT_QUOTES = {'“': '"', '”': '"', '‘': "'", '’': "'"}


def normalize_encoding(text, encoding, straight_quotes=False):
    """Make text encodable in the game's script encoding."""
    if straight_quotes:
        for a, b in STRAIGHT_QUOTES.items():
            text = text.replace(a, b)
    if not encoding or encoding.lower() in ('utf-8', 'utf8', 'utf-16', 'utf16', 'unicode'):
        return text
    out = []
    for ch in text:
        ch = SJIS_REPLACE.get(ch, ch)
        try:
            ch.encode(encoding)
            out.append(ch)
        except UnicodeEncodeError:
            base = unicodedata.normalize('NFKD', ch).encode('ascii', 'ignore').decode('ascii')
            out.append(base or '?')
    return ''.join(out)


def fit(text, box, line_break='\n'):
    """Wrap text for a box. Returns (wrapped, overflow_lines)."""
    # In 'auto' mode the engine wraps on its own, but usually mid-word, so we
    # still insert breaks unless the user turned that off.
    if box.get('insert_breaks') is False:
        lines = max(1, -(-text_cells(text) // max(1, box['width'])))
        return text, max(0, lines - box['lines'])
    wrapped, n = wrap(text, box['width'], line_break)
    return wrapped, max(0, n - box['lines'])
