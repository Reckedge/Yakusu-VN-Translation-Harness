"""The translation pipeline: extract -> style -> translate -> review -> fit -> build -> install.

Everything is resumable: results are appended as they arrive, so stopping
(or a crash, or a rate limit) loses at most the batches in flight.
"""
import concurrent.futures as cf
import json
import math
import os
import re
import shutil
import threading
import time
from collections import Counter

from .. import engines
from ..engines.base import EngineError, list_files, read_json_messages, write_json_messages
from . import llm, prompts, textfit
from .protect import has_japanese, protect, restore, check

# Strings that are clearly not dialogue: file names, identifiers, paths.
NOT_TEXT_RE = re.compile(r'^[A-Za-z0-9_\-./\\:*]+$|\.(png|jpg|ogg|wav|mp3|ks|tjs|bmp|tlg|psb|txt|dat)\b', re.I)

QUALITY_PRESETS = {
    'quick': {'label': 'Quick', 'translate': 'claude-haiku-5-5', 'review': None,
              'blurb': 'Haiku translates everything. Cheapest; good enough to follow the story.'},
    'recommended': {'label': 'Recommended', 'translate': 'claude-haiku-5-5', 'review': 'claude-sonnet-5-5',
                    'blurb': 'Haiku translates, then Sonnet edits every line. Reads like a fan translation.'},
    'best': {'label': 'Best', 'translate': 'claude-sonnet-5-5', 'review': 'claude-opus-5-5',
             'blurb': 'Sonnet translates and Opus edits. Highest quality, highest cost.'},
}


class Cancelled(Exception):
    pass


class BudgetReached(Exception):
    pass


class Pipeline:
    def __init__(self, project, settings, log=print, progress=None, cancel=None):
        self.p = project
        self.s = settings
        self.log = log
        self.progress = progress or (lambda **kw: None)
        self.cancel = cancel or threading.Event()

    # ---------------------------------------------------------------- engine
    def engine(self):
        m = self.p.meta.get('engine')
        if not m:
            raise EngineError('Choose a game folder first.')
        return engines.engine_for(m)

    def _save_engine(self, eng):
        self.p.meta['engine']['details'] = eng.details
        self.p.save_meta()

    # ---------------------------------------------------------------- extract
    def extract(self):
        eng = self.engine()
        self.progress(stage='extract', done=0, total=0, message='Reading the game\'s scripts')
        json_dir = eng.prepare(self.p, self.log)
        self._save_engine(eng)
        lines = []
        speakers = Counter()
        for rel in list_files(json_dir, {'.json'}):
            rel_n = rel.replace('\\', '/')
            for i, e in enumerate(read_json_messages(os.path.join(json_dir, rel))):
                jp = e.get('message') or ''
                name = e.get('name') or ((e.get('names') or [None])[0])
                pr = protect(jp, eng.line_break_for(rel_n))
                if pr.name_prefix and not name:
                    name = pr.name_prefix[1]
                skip = ''
                if not has_japanese(pr.text):
                    skip = 'no Japanese'
                elif NOT_TEXT_RE.search(pr.text.strip()):
                    skip = 'looks like a file name'
                lines.append({'id': f'{rel_n}#{i}', 'file': rel_n, 'idx': i, 'name': name, 'jp': jp,
                              'skip': skip})
                if name and not skip:
                    speakers[name] += 1
        if not any(not l['skip'] for l in lines):
            raise EngineError('The scripts were read, but no Japanese text was found in them.')
        self.p.set_lines(lines)
        live = [l for l in lines if not l['skip']]
        box = textfit.measure_box([l['jp'].replace(eng.line_break_for(l['file']), '\n') for l in live], '\n')
        old = self.p.meta.get('box') or {}
        if old.get('user_set'):
            box = old
        self.p.meta['box'] = box
        self.p.meta['stats'] = {
            'lines': len(live), 'skipped': len(lines) - len(live),
            'files': len({l['file'] for l in lines}),
            'jp_chars': sum(len(protect(l['jp'], eng.line_break_for(l['file'])).text) for l in live),
            'speakers': speakers.most_common(),
        }
        self.p.meta['extracted_at'] = time.time()
        self.p.save_meta()
        self.log(f'Found {len(live):,} lines of text in {self.p.meta["stats"]["files"]} files.')
        return self.p.meta['stats']

    # ---------------------------------------------------------------- helpers
    def live_lines(self):
        return [l for l in self.p.lines() if not l.get('skip')]

    def models(self):
        prov = self.s.get('provider', 'anthropic')
        preset = self.s.get('preset', 'recommended')
        if prov == 'anthropic' and preset in QUALITY_PRESETS:
            q = QUALITY_PRESETS[preset]
            return q['translate'], (q['review'] if self.s.get('review', True) else None)
        tm = (self.s.get('translate_model') or {}).get(prov) or ''
        rm = (self.s.get('review_model') or {}).get(prov) or ''
        return tm, (rm if (rm and self.s.get('review', True)) else None)

    def client(self, model):
        prov = self.s.get('provider', 'anthropic')
        key = (self.s.get('keys') or {}).get(prov, '')
        if prov != 'local' and not key:
            raise EngineError('Add your API key first.')
        if not model:
            raise EngineError('Choose a model first.')
        base = (self.s.get('base_urls') or {}).get(prov) or None
        return llm.Client(prov, key, model, base)

    def _who(self, line, names):
        n = line.get('name')
        if not n:
            return 'narration'
        return names.get(n) or n

    def _check_budget(self):
        cap = float(self.s.get('budget_usd') or 0)
        if cap > 0 and self.p.spent()[0] >= cap:
            raise BudgetReached(f'Stopped at your ${cap:.2f} spending limit. Raise it in Settings to continue.')

    def _call(self, client, stage, system, user, max_tokens):
        if self.cancel.is_set():
            raise Cancelled()
        self._check_budget()
        try:
            text, usage, usd = client.complete(system, user, max_tokens=max_tokens)
        except llm.Truncated as t:
            usage = t.args[0] if t.args else {}
            self.p.log_cost(stage, client.model, usage, llm.cost_usd(client.model, usage))
            raise
        self.p.log_cost(stage, client.model, usage, usd)
        return text

    # ---------------------------------------------------------------- estimate
    def estimate(self):
        """Rough cost range in USD for each stage, from character counts.

        Calibrated against a 53,000-line VN translated with Haiku + a Sonnet
        review (~$31 actual). Shown as a range because output length varies.
        """
        st = self.p.meta.get('stats') or {}
        n = st.get('lines', 0)
        chars = st.get('jp_chars', 0)
        done = self.p.results()
        live = self.live_lines()
        todo = [l for l in live if not (done.get(l['id']) or {}).get('en_ph')]
        frac = (len(todo) / n) if n else 1
        tm, rm = self.models()
        bs = int(self.s.get('batch_size', 40))
        ctx = int(self.s.get('context_lines', 10))
        sys_tokens = 700 + len(self.p.style()) // 3 + len(json.dumps(self.p.glossary(), ensure_ascii=False)) // 2
        batches = math.ceil(n / bs) if n else 0
        tr_in = chars * 1.3 + n * 22 + (ctx / bs) * (chars * 1.3 + chars * 0.6 + n * 25)
        tr_out = chars * 0.62 + n * 12

        def cost(model, tin, tout, sys_reads):
            p = llm.price_of(model)
            if not p:
                return None
            return (tin * p[0] + tout * p[1] + sys_reads * sys_tokens * p[3] + sys_tokens * p[2]) / 1e6

        out = {'lines': n, 'todo': len(todo), 'translate_model': tm, 'review_model': rm}
        c = cost(tm, tr_in * frac, tr_out * frac, batches * frac)
        out['translate'] = None if c is None else [round(c, 4), round(c * 1.6, 4)]
        if rm:
            rv_in = chars * 1.3 + chars * 0.62 + n * 30
            rv_out = (chars * 0.62 + n * 12) * 0.4
            c2 = cost(rm, rv_in, rv_out, math.ceil(n / 30))
            out['review'] = None if c2 is None else [round(c2, 4), round(c2 * 1.6, 4)]
        else:
            out['review'] = [0, 0]
        if out['translate'] is not None and out['review'] is not None:
            out['total'] = [round(out['translate'][0] + out['review'][0], 4),
                            round(out['translate'][1] + out['review'][1], 4)]
        else:
            out['total'] = None
        out['spent'] = round(self.p.spent()[0], 4)
        return out

    # ---------------------------------------------------------------- style
    def draft_style(self):
        live = self.live_lines()
        if not live:
            raise EngineError('Read the game\'s scripts first.')
        tm, rm = self.models()
        client = self.client(rm or tm)
        self.progress(stage='style', done=0, total=1, message='Reading a sample of the script')
        eng = self.engine()
        sample = live[:150]
        rest = live[150:]
        if rest:
            step = max(1, len(rest) // 250)
            sample += rest[::step][:250]
        speakers = (self.p.meta.get('stats') or {}).get('speakers') or []
        user = 'Speaker names (with line counts):\n' + '\n'.join(f'- {n} ({c})' for n, c in speakers[:80])
        user += '\n\nScript sample:\n' + '\n'.join(
            f'[{l.get("name") or "narration"}] {protect(l["jp"], eng.line_break_for(l["file"])).text}' for l in sample)
        text = self._call(client, 'style', prompts.STYLE_SYSTEM, user, 6000)
        data = _parse_object(text)
        style = data.get('style_sheet', '').strip()
        if data.get('summary'):
            style = f'{data["summary"].strip()}\n\n{style}'
        self.p.set_style(style)
        names = {}
        for n in data.get('names') or []:
            if n.get('jp') and n.get('en'):
                names[n['jp']] = n['en']
        g = self.p.glossary()
        g['names'] = {**names, **{k: v for k, v in (g.get('names') or {}).items() if v}}
        terms = {t['jp']: t for t in (g.get('terms') or []) if t.get('jp')}
        for t in data.get('terms') or []:
            if t.get('jp') and t.get('en') and t['jp'] not in terms:
                terms[t['jp']] = {'jp': t['jp'], 'en': t['en']}
        g['terms'] = list(terms.values())
        self.p.set_glossary(g)
        self.p.meta['style_at'] = time.time()
        self.p.save_meta()
        self.progress(stage='style', done=1, total=1, message='Style sheet ready')
        return {'style': style, 'glossary': g}

    # ---------------------------------------------------------------- names
    def missing_names(self):
        names = self.p.glossary().get('names') or {}
        speakers = (self.p.meta.get('stats') or {}).get('speakers') or []
        return [n for n, _ in speakers if not names.get(n)]

    def ensure_names(self, client):
        """Give every speaker an English name before translating, so name
        boxes and pronouns stay consistent even if the style step was skipped."""
        missing = self.missing_names()
        if not missing:
            return
        self.log(f'Naming {len(missing)} speakers without an English name.')
        try:
            text = self._call(client, 'names', prompts.NAMES_SYSTEM,
                              '\n'.join(missing[:300]), 40 * min(len(missing), 300) + 400)
            arr = llm.parse_json_array(text)
        except (ValueError, TypeError, llm.Truncated):
            self.log('Couldn\'t get English names; speaker names stay in Japanese for now.')
            return
        g = self.p.glossary()
        names = dict(g.get('names') or {})
        for o in arr:
            if isinstance(o, dict) and o.get('jp') in missing and o.get('en') and not names.get(o['jp']):
                names[o['jp']] = str(o['en']).strip()
        self.p.set_glossary({**g, 'names': names})

    # ---------------------------------------------------------------- translate
    def translate(self, ids=None, redo=False):
        eng = self.engine()
        tm, _ = self.models()
        client = self.client(tm)
        self.ensure_names(client)
        g = self.p.glossary()
        names = g.get('names') or {}
        system = prompts.TRANSLATE_SYSTEM.format(style=prompts.style_block(self.p.style()),
                                                 glossary=prompts.glossary_block(g))
        live = self.live_lines()
        index = {l['id']: k for k, l in enumerate(live)}
        want = set(ids) if ids else None
        done = self.p.results()
        todo = [l for l in live if (want is None or l['id'] in want)
                and (redo or not (done.get(l['id']) or {}).get('en_ph') or done[l['id']].get('problems'))]
        if not todo:
            self.log('Nothing left to translate in this scope.')
            return 0
        # Translation memory: a line identical to one already translated (same
        # text, same speaker) reuses that translation. VNs repeat short lines
        # ("……", "Yeah.", choices) a lot, so this saves money and keeps them consistent.
        def key(l):
            return (protect(l['jp'], eng.line_break_for(l['file'])).text, l.get('name') or '')
        known = {}
        if not redo:
            for l in live:
                r = done.get(l['id'])
                if r and r.get('en_ph') and not r.get('problems'):
                    known.setdefault(key(l), r['en_ph'])
        unique, copies, first = [], [], {}
        for l in todo:
            k = key(l)
            if k in known:
                copies.append((l, None, known[k]))
            elif k in first:
                copies.append((l, first[k], None))
            else:
                first[k] = l
                unique.append(l)
        reuse_now = [self._row(l, en, client, 'translate') | {'model': 'memory'} for l, _, en in copies if en]
        if reuse_now:
            self.p.add_results(reuse_now)
            self.log(f'Reused earlier translations for {len(reuse_now):,} repeated lines.')
        todo = unique
        pending_copies = [(l, src) for l, src, en in copies if src is not None]
        if not todo:
            return len(reuse_now)
        bs = int(self.s.get('batch_size', 40))
        ctx_n = int(self.s.get('context_lines', 10))
        batches = [todo[i:i + bs] for i in range(0, len(todo), bs)]
        total = len(todo)
        state = {'done': 0}
        lock = threading.Lock()
        self.progress(stage='translate', done=0, total=total, message=f'Translating with {tm}')

        def run(batch):
            first = index[batch[0]['id']]
            ctx = live[max(0, first - ctx_n):first]
            rows = self._translate_batch(client, system, batch, ctx, names, eng)
            self.p.add_results(rows)
            with lock:
                state['done'] += len(batch)
                spent = self.p.spent()[0]
                self.progress(stage='translate', done=state['done'], total=total,
                              message=f'Translating with {tm}', spent=spent)

        self._run_parallel(run, batches)
        if pending_copies:
            res = self.p.results()
            rows = []
            for l, src in pending_copies:
                r = res.get(src['id'])
                if r and r.get('en_ph') and not r.get('problems'):
                    rows.append(self._row(l, r['en_ph'], client, 'translate') | {'model': 'memory'})
            self.p.add_results(rows)
            if rows:
                self.log(f'Reused translations for {len(rows):,} repeated lines.')
        return total + len(reuse_now) + len(pending_copies)

    def _translate_batch(self, client, system, batch, ctx, names, eng):
        """Translate a batch. Unreadable or cut-off replies split the batch in
        half; lines that come back broken are retried, then retried alone."""
        results = self.p.results()
        prot = [protect(l['jp'], eng.line_break_for(l['file'])) for l in batch]
        got = self._ask_translate(client, system, batch, prot, ctx, names, eng, results)
        if got is None:  # reply unusable for the whole batch
            if len(batch) > 1:
                mid = len(batch) // 2
                self.log(f'Splitting a batch of {len(batch)} lines after an unusable reply.')
                first = self._translate_batch(client, system, batch[:mid], ctx, names, eng)
                ctx2 = (list(ctx) + batch[:mid])[-max(len(ctx), 5):]
                return first + self._translate_batch(client, system, batch[mid:], ctx2, names, eng)
            got = {}
        rows, retry = [], []
        for k, (l, pr) in enumerate(zip(batch, prot)):
            en = got.get(k + 1)
            probs = check(pr, en) if en is not None else ['no answer']
            if probs:
                retry.append((l, pr, probs))
            else:
                rows.append(self._row(l, en, client, 'translate'))
        if len(retry) > 2:
            # One more try for the broken lines together before going one by one.
            sub = [r[0] for r in retry]
            subp = [r[1] for r in retry]
            got2 = self._ask_translate(client, system, sub, subp, ctx, names, eng, results,
                                       note='Some of these lines came back broken last time. Keep every placeholder.') or {}
            still = []
            for k, (l, pr, probs) in enumerate(retry):
                en = got2.get(k + 1)
                p2 = check(pr, en) if en is not None else ['no answer']
                if p2:
                    still.append((l, pr, p2))
                else:
                    rows.append(self._row(l, en, client, 'translate'))
            retry = still
        for l, pr, probs in retry:
            en, probs2 = self._translate_single(client, system, l, pr, names, probs)
            rows.append(self._row(l, en, client, 'translate', probs2))
        return rows

    def _row(self, line, en, client, stage, problems=None):
        r = {'id': line['id'], 'en_ph': (en or '').strip(), 'stage': stage, 'model': client.model, 'src': line['jp']}
        if problems:
            r['problems'] = problems
        return r

    def _ask_translate(self, client, system, batch, prot, ctx, names, eng, results, note=''):
        """One model call. Returns {index: text}, or None if the reply was unusable."""
        context = []
        for l in ctx:
            r = results.get(l['id'])
            context.append({'who': self._who(l, names), 'jp': protect(l['jp'], eng.line_break_for(l['file'])).text,
                            **({'en': r['en_ph']} if r and r.get('en_ph') else {})})
        items = [{'i': k + 1, 'who': self._who(l, names), 'jp': pr.text} for k, (l, pr) in enumerate(zip(batch, prot))]
        user = ''
        if context:
            user += 'CONTEXT (earlier lines, do not translate):\n' + '\n'.join(json.dumps(c, ensure_ascii=False) for c in context) + '\n\n'
        if note:
            user += note + '\n\n'
        user += 'TRANSLATE:\n' + '\n'.join(json.dumps(it, ensure_ascii=False) for it in items)
        budget = sum(len(pr.text) for pr in prot) * 3 + 60 * len(batch) + 400
        try:
            text = self._call(client, 'translate', system, user, max(1500, min(budget, 16000)))
            arr = llm.parse_json_array(text)
        except llm.Truncated:
            self.log('A reply was cut off by the length limit.')
            return None
        except (ValueError, TypeError) as e:
            self.log(f'Couldn\'t read a reply ({e}).')
            return None
        got = {}
        for o in arr:
            if isinstance(o, dict) and 'i' in o:
                try:
                    got[int(o['i'])] = str(o.get('en', ''))
                except (TypeError, ValueError):
                    continue
        if not got and batch:
            return None
        return got

    def _translate_single(self, client, system, l, pr, names, probs):
        user = ('TRANSLATE (the last attempt had problems: ' + ', '.join(probs) + '):\n'
                + json.dumps({'i': 1, 'who': self._who(l, names), 'jp': pr.text}, ensure_ascii=False))
        en = ''
        for attempt in range(2):
            try:
                text = self._call(client, 'translate', system, user, max(800, len(pr.text) * 4 + 200))
                arr = llm.parse_json_array(text)
                en = str(arr[0].get('en', '')) if arr else ''
            except (ValueError, TypeError, IndexError, AttributeError, llm.Truncated):
                en = ''
            p2 = check(pr, en)
            if not p2:
                return en.strip(), []
        return (en or '').strip(), check(pr, en) or ['failed']

    # ---------------------------------------------------------------- review
    def review(self, ids=None):
        _, rm = self.models()
        if not rm:
            self.log('Review is off.')
            return 0
        eng = self.engine()
        client = self.client(rm)
        g = self.p.glossary()
        names = g.get('names') or {}
        system = prompts.REVIEW_SYSTEM.format(style=prompts.style_block(self.p.style()),
                                              glossary=prompts.glossary_block(g))
        results = self.p.results()
        want = set(ids) if ids else None
        todo = [l for l in self.live_lines() if (want is None or l['id'] in want)
                and l['id'] in results and results[l['id']].get('stage') == 'translate'
                and results[l['id']].get('en_ph')]
        if not todo:
            self.log('Nothing to review yet.')
            return 0
        batches = [todo[i:i + 30] for i in range(0, len(todo), 30)]
        total = len(todo)
        state = {'done': 0, 'changed': 0}
        lock = threading.Lock()
        self.progress(stage='review', done=0, total=total, message=f'Reviewing with {rm}')

        def review_batch(batch):
            prot = [protect(l['jp'], eng.line_break_for(l['file'])) for l in batch]
            items = [{'i': k + 1, 'who': self._who(l, names), 'jp': pr.text, 'en': results[l['id']]['en_ph']}
                     for k, (l, pr) in enumerate(zip(batch, prot))]
            user = '\n'.join(json.dumps(it, ensure_ascii=False) for it in items)
            budget = sum(len(it['en']) for it in items) + 40 * len(batch) + 400
            try:
                text = self._call(client, 'review', system, user, max(1500, min(budget, 16000)))
                arr = llm.parse_json_array(text)
            except (llm.Truncated, ValueError, TypeError):
                if len(batch) > 1:
                    mid = len(batch) // 2
                    return review_batch(batch[:mid]) + review_batch(batch[mid:])
                return []  # leave this line unreviewed; a later review run picks it up
            changed = {}
            for o in arr:
                if isinstance(o, dict) and 'i' in o:
                    try:
                        changed[int(o['i'])] = str(o.get('en', ''))
                    except (TypeError, ValueError):
                        pass
            rows = []
            for k, (l, pr) in enumerate(zip(batch, prot)):
                before = results[l['id']]['en_ph']
                en = changed.get(k + 1)
                if en is not None and en.strip() != before and not check(pr, en):
                    rows.append({'id': l['id'], 'en_ph': en.strip(), 'stage': 'review', 'model': client.model,
                                 'src': l['jp'], 'before': before})
                else:
                    rows.append({**results[l['id']], 'stage': 'reviewed', 'model_review': client.model})
            return rows

        def run(batch):
            rows = review_batch(batch)
            self.p.add_results(rows)
            with lock:
                state['done'] += len(batch)
                state['changed'] += sum(1 for r in rows if r['stage'] == 'review')
                self.progress(stage='review', done=state['done'], total=total,
                              message=f'Reviewing with {rm} ({state["changed"]} lines improved)',
                              spent=self.p.spent()[0])

        self._run_parallel(run, batches)
        return total

    # ---------------------------------------------------------------- fit
    def render(self, line, result, eng=None, box=None):
        """Final message text for one line, plus overflow count."""
        eng = eng or self.engine()
        box = box or self.p.meta.get('box') or {'mode': 'auto', 'width': 56, 'lines': 3}
        settings = self.p.meta.get('settings') or {}
        g = self.p.glossary()
        names = g.get('names') or {}
        lb = eng.line_break_for(line['file'])
        pr = protect(line['jp'], lb)
        qmode = settings.get('quotes', 'auto')
        if qmode == 'keep':
            quotes = None
        elif qmode == 'none':
            quotes = ('', '')
        elif qmode == 'curly' or (qmode == 'auto' and not eng.straight_quotes):
            quotes = ('“', '”')
        else:
            quotes = ('"', '"')
        en_name = names.get(pr.name_prefix[1]) if pr.name_prefix else None
        text = restore(pr, result['en_ph'], quotes=quotes, name=en_name)
        text = textfit.normalize_encoding(text, eng.encoding, straight_quotes=eng.straight_quotes and qmode == 'auto')
        if settings.get('insert_breaks', True) is False:
            fitted, over = textfit.fit(text, {**box, 'insert_breaks': False}, lb)
        else:
            fitted, over = textfit.fit(text, box, lb)
        return fitted, over

    def overflow_lines(self):
        eng = self.engine()
        box = self.p.meta.get('box')
        res = self.p.results()
        out = []
        for l in self.live_lines():
            r = res.get(l['id'])
            if r and r.get('en_ph'):
                _, over = self.render(l, r, eng, box)
                if over > 0:
                    out.append(l['id'])
        return out

    def shorten(self, ids=None):
        eng = self.engine()
        box = self.p.meta.get('box')
        ids = ids or self.overflow_lines()
        if not ids:
            self.log('Every line fits the text box.')
            return 0
        tm, rm = self.models()
        client = self.client(rm or tm)
        res = self.p.results()
        by_id = {l['id']: l for l in self.live_lines()}
        todo = [by_id[i] for i in ids if i in by_id and i in res]
        names = self.p.glossary().get('names') or {}

        def limit_for(l):
            pr = protect(l['jp'], eng.line_break_for(l['file']))
            room = int(box['width'] * box['lines'] * 0.92)
            if pr.open_quote:
                room -= 2
            if pr.name_prefix:
                room -= len(names.get(pr.name_prefix[1]) or pr.name_prefix[1]) + 2
            return max(10, room)
        batches = [todo[i:i + 30] for i in range(0, len(todo), 30)]
        state = {'done': 0}
        lock = threading.Lock()
        self.progress(stage='fit', done=0, total=len(todo), message='Shortening lines that overflow the text box')

        def run(batch):
            prot = [protect(l['jp'], eng.line_break_for(l['file'])) for l in batch]
            items = [{'i': k + 1, 'max': limit_for(l), 'en': res[l['id']]['en_ph']} for k, l in enumerate(batch)]
            user = '\n'.join(json.dumps(it, ensure_ascii=False) for it in items)
            try:
                text = self._call(client, 'fit', prompts.SHORTEN_SYSTEM, user, max(1200, 100 * len(batch)))
                arr = llm.parse_json_array(text)
            except (ValueError, TypeError, llm.Truncated):
                arr = []
            rows = []
            for o in arr:
                if not isinstance(o, dict) or 'i' not in o:
                    continue
                k = int(o['i']) - 1
                if 0 <= k < len(batch) and not check(prot[k], o.get('en', '')):
                    l = batch[k]
                    rows.append({'id': l['id'], 'en_ph': str(o['en']).strip(), 'stage': 'fit', 'model': client.model,
                                 'src': l['jp'], 'before': res[l['id']]['en_ph']})
            self.p.add_results(rows)
            with lock:
                state['done'] += len(batch)
                self.progress(stage='fit', done=state['done'], total=len(todo),
                              message='Shortening lines that overflow the text box', spent=self.p.spent()[0])

        self._run_parallel(run, batches)
        return len(todo)

    # ---------------------------------------------------------------- checks
    def glossary_issues(self):
        """Translated lines whose Japanese uses a glossary name/term but whose
        English doesn't use the agreed rendering. {line id: [missing English]}"""
        g = self.p.glossary()
        pairs = [(jp, en) for jp, en in (g.get('names') or {}).items() if jp and en and len(jp) >= 2]
        pairs += [(t['jp'], t['en']) for t in (g.get('terms') or []) if t.get('jp') and t.get('en') and len(t['jp']) >= 2]
        if not pairs:
            return {}
        res = self.p.results()
        out = {}
        for l in self.live_lines():
            r = res.get(l['id'])
            if not r or not r.get('en_ph'):
                continue
            en = r['en_ph'].lower()
            miss = []
            for jp, word in pairs:
                if jp in l['jp']:
                    key = word.lower().split()[0].strip('"\'.,!?')
                    if key and key not in en:
                        miss.append(word)
            if miss:
                out[l['id']] = miss
        return out

    def fix_glossary(self):
        """Ask the editor model to fix lines that don't use the glossary's spellings."""
        issues = self.glossary_issues()
        if not issues:
            return 0
        eng = self.engine()
        tm, rm = self.models()
        client = self.client(rm or tm)
        g = self.p.glossary()
        names = g.get('names') or {}
        system = prompts.REVIEW_SYSTEM.format(style=prompts.style_block(self.p.style()),
                                              glossary=prompts.glossary_block(g))
        res = self.p.results()
        by_id = {l['id']: l for l in self.live_lines()}
        todo = [by_id[i] for i in issues if i in by_id]
        batches = [todo[i:i + 30] for i in range(0, len(todo), 30)]
        state = {'done': 0}
        lock = threading.Lock()
        self.progress(stage='glossary', done=0, total=len(todo), message='Fixing name and term spellings')

        def run(batch):
            prot = [protect(l['jp'], eng.line_break_for(l['file'])) for l in batch]
            items = [{'i': k + 1, 'who': self._who(l, names), 'jp': pr.text, 'en': res[l['id']]['en_ph'],
                      'must_use': issues[l['id']]} for k, (l, pr) in enumerate(zip(batch, prot))]
            user = ('Each line below must use the English spellings listed in "must_use". '
                    'Rewrite only what is needed to use them.\n' + '\n'.join(json.dumps(it, ensure_ascii=False) for it in items))
            try:
                arr = llm.parse_json_array(self._call(client, 'glossary', system, user, max(1500, 120 * len(batch))))
            except (ValueError, TypeError, llm.Truncated):
                arr = []
            rows = []
            for o in arr:
                try:
                    k = int(o.get('i')) - 1
                except (TypeError, ValueError, AttributeError):
                    continue
                if 0 <= k < len(batch) and o.get('en') and not check(prot[k], o['en']):
                    l = batch[k]
                    rows.append({'id': l['id'], 'en_ph': str(o['en']).strip(), 'stage': 'review', 'model': client.model,
                                 'src': l['jp'], 'before': res[l['id']]['en_ph']})
            self.p.add_results(rows)
            with lock:
                state['done'] += len(batch)
                self.progress(stage='glossary', done=state['done'], total=len(todo),
                              message='Fixing name and term spellings', spent=self.p.spent()[0])

        self._run_parallel(run, batches)
        return len(todo)

    def preflight(self):
        """Things worth knowing before a full run, as [{level, text}]."""
        notes = []
        m = self.p.meta
        eng = m.get('engine') or {}
        prov = self.s.get('provider', 'anthropic')
        if prov != 'local' and not (self.s.get('keys') or {}).get(prov):
            notes.append({'level': 'warn', 'text': 'Add your API key in step 2 before translating.'})
        tm, _ = self.models()
        if not tm:
            notes.append({'level': 'warn', 'text': 'Choose a translation model in step 2.'})
        if eng.get('support') == 'experimental':
            notes.append({'level': 'warn', 'text': f'{eng.get("name")} support is experimental. Make a test patch '
                          'and check it in the game before translating everything.'})
        if not self.p.style().strip():
            notes.append({'level': 'warn', 'text': 'No style sheet yet. Drafting one (step 3, a few cents) keeps '
                          'names, tone and the narrator\'s voice consistent.'})
        missing = self.missing_names()
        if missing:
            notes.append({'level': 'info', 'text': f'{len(missing)} speaker{"s have" if len(missing) != 1 else " has"} '
                          'no English name yet. Yakusu will name them before translating.'})
        if not m.get('built_at'):
            notes.append({'level': 'info', 'text': 'You haven\'t made a test patch. It\'s the cheapest way to check '
                          'that text shows up correctly in this game.'})
        try:
            est = self.estimate()
            cap = float(self.s.get('budget_usd') or 0)
            if cap and est.get('total') and est['total'][0] + est['spent'] > cap:
                notes.append({'level': 'warn', 'text': f'Your ${cap:.2f} spending limit is probably lower than this '
                              'run will cost. Yakusu will stop when it reaches it; progress is kept.'})
        except Exception:
            pass
        return notes

    # ---------------------------------------------------------------- build / install
    def build(self):
        eng = self.engine()
        box = self.p.meta.get('box')
        res = self.p.results()
        g = self.p.glossary()
        names = g.get('names') or {}
        src = self.p.work('json')
        dst = self.p.work('json_tl')
        if os.path.isdir(dst):
            shutil.rmtree(dst)
        by_id = {l['id']: l for l in self.p.lines()}
        applied = 0
        self.progress(stage='build', done=0, total=0, message='Writing the translation into the scripts')
        for rel in list_files(src, {'.json'}):
            rel_n = rel.replace('\\', '/')
            entries = read_json_messages(os.path.join(src, rel))
            for i, e in enumerate(entries):
                lid = f'{rel_n}#{i}'
                l = by_id.get(lid)
                r = res.get(lid)
                if l and r and r.get('en_ph') and not l.get('skip') and r.get('src', l['jp']) == l['jp']:
                    e['message'], _ = self.render(l, r, eng, box)
                    applied += 1
                if e.get('name') and names.get(e['name']):
                    e['name'] = textfit.normalize_encoding(names[e['name']], eng.encoding, eng.straight_quotes)
                if e.get('names'):
                    e['names'] = [textfit.normalize_encoding(names.get(n, n), eng.encoding, eng.straight_quotes)
                                  for n in e['names']]
            write_json_messages(os.path.join(dst, rel), entries)
        if not applied:
            raise EngineError('No translated lines yet. Translate a few lines first.')
        out = eng.build(self.p, self.log)
        self._save_engine(eng)
        self.p.meta['built_at'] = time.time()
        self.p.meta['built_lines'] = applied
        self.p.save_meta()
        self.log(f'Built a patch with {applied:,} translated lines.')
        return {'out': out, 'applied': applied, 'manual_output': eng.details.get('manual_output')}

    def install(self):
        eng = self.engine()
        if eng.details.get('manual_output') and not list_files(self.p.work('out')):
            raise EngineError('This engine needs a manual step: the patched files are in '
                              f'{eng.details["manual_output"]}.')
        files = eng.install(self.p, self.log)
        self.p.meta['installed_at'] = time.time()
        self.p.save_meta()
        return files

    def restore(self):
        eng = self.engine()
        eng.restore(self.p, self.log)
        self.p.meta['installed_at'] = None
        self.p.save_meta()

    # ---------------------------------------------------------------- runner
    def _run_parallel(self, fn, batches):
        workers = max(1, int(self.s.get('parallel', 4)))
        if self.s.get('provider') == 'local':
            workers = 1
        errors = []
        with cf.ThreadPoolExecutor(workers) as ex:
            futs = []
            for b in batches:
                futs.append(ex.submit(fn, b))
            for f in cf.as_completed(futs):
                try:
                    f.result()
                except (Cancelled, BudgetReached, llm.LLMError) as e:
                    self.cancel.set()
                    errors.append(e)
                except Exception as e:  # keep going; unexpected errors surface at the end
                    errors.append(e)
                    self.log(f'Batch failed: {e}')
        # Report the root cause, not the cancellations it triggered.
        for e in errors:
            if isinstance(e, (BudgetReached, llm.LLMError)):
                raise e
        for e in errors:
            if not isinstance(e, Cancelled):
                raise e
        if errors:
            raise errors[0]


def _parse_object(text):
    t = text.strip()
    a, b = t.find('{'), t.rfind('}')
    if a < 0 or b < a:
        raise EngineError('The model didn\'t return a style sheet. Try again.')
    return json.loads(t[a:b + 1])
