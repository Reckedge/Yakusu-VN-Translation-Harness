"""A translation project: one per game folder.

Everything lives in the user's app-data folder, never inside the game,
except what the installer copies in (and records for restore).

  project.json   game path, engine, text box, settings, progress flags
  lines.jsonl    source lines: id, file, idx, name, jp
  results.jsonl  translations, append-only; the LAST entry for an id wins
  costs.jsonl    one entry per API call
  style.md       style sheet (editable)
  glossary.json  {"names": {jp: en}, "terms": [{jp, en}]}
  work/          engine scratch space (unpacked scripts, JSON, patched output)
"""
import hashlib
import json
import os
import re
import threading
import time

from .. import config


def slugify(path):
    base = re.sub(r'[^A-Za-z0-9._-]+', '-', os.path.basename(os.path.normpath(path)) or 'game').strip('-')[:40]
    h = hashlib.sha1(os.path.normcase(os.path.abspath(path)).encode('utf-8')).hexdigest()[:8]
    return f'{base or "game"}-{h}'


class Project:
    def __init__(self, root):
        self.root = root
        self.lock = threading.RLock()
        self._lines = None
        self._results = None
        self._spent = None        # (total, by_stage), kept in memory once read
        self._glossary = None
        os.makedirs(root, exist_ok=True)
        self.meta = self._read_json('project.json', {})

    # -- creation ------------------------------------------------------
    @classmethod
    def for_game(cls, game_dir):
        root = os.path.join(config.projects_dir(), slugify(game_dir))
        p = cls(root)
        if not p.meta.get('game_dir'):
            p.meta.update({'game_dir': os.path.abspath(game_dir), 'created': time.time(),
                           'settings': {'quotes': 'auto', 'insert_breaks': True}})
            p.save_meta()
        return p

    # -- paths ---------------------------------------------------------
    def path(self, *parts):
        return os.path.join(self.root, *parts)

    def work(self, *parts):
        p = os.path.join(self.root, 'work', *parts)
        return p

    @property
    def game_dir(self):
        return self.meta.get('game_dir')

    # -- json helpers --------------------------------------------------
    def _read_json(self, name, default):
        try:
            with open(self.path(name), encoding='utf-8') as f:
                return json.load(f)
        except (OSError, ValueError):
            return default

    def _write_json(self, name, data):
        tmp = self.path(name + '.tmp')
        with open(tmp, 'w', encoding='utf-8') as f:
            json.dump(data, f, ensure_ascii=False, indent=2)
        os.replace(tmp, self.path(name))

    def save_meta(self):
        with self.lock:
            self._write_json('project.json', self.meta)

    # -- lines ---------------------------------------------------------
    def set_lines(self, lines):
        with self.lock:
            with open(self.path('lines.jsonl'), 'w', encoding='utf-8') as f:
                for ln in lines:
                    f.write(json.dumps(ln, ensure_ascii=False) + '\n')
            self._lines = lines

    def lines(self):
        with self.lock:
            if self._lines is None:
                self._lines = []
                if os.path.exists(self.path('lines.jsonl')):
                    with open(self.path('lines.jsonl'), encoding='utf-8') as f:
                        self._lines = [json.loads(l) for l in f if l.strip()]
            return self._lines

    # -- results -------------------------------------------------------
    def results(self):
        with self.lock:
            if self._results is None:
                self._results = {}
                if os.path.exists(self.path('results.jsonl')):
                    with open(self.path('results.jsonl'), encoding='utf-8') as f:
                        for l in f:
                            if l.strip():
                                try:
                                    r = json.loads(l)
                                except ValueError:
                                    continue  # a half-written last line after a crash
                                self._results[r['id']] = r
            return self._results

    def add_results(self, rows):
        with self.lock:
            res = self.results()
            with open(self.path('results.jsonl'), 'a', encoding='utf-8') as f:
                for r in rows:
                    r.setdefault('t', time.time())
                    f.write(json.dumps(r, ensure_ascii=False) + '\n')
                    res[r['id']] = r

    # -- costs ---------------------------------------------------------
    def log_cost(self, stage, model, usage, usd):
        with self.lock:
            total, by_stage = self.spent()
            with open(self.path('costs.jsonl'), 'a', encoding='utf-8') as f:
                f.write(json.dumps({'t': time.time(), 'stage': stage, 'model': model,
                                    'usage': usage, 'usd': usd}) + '\n')
            by_stage = dict(by_stage)
            by_stage[stage] = by_stage.get(stage, 0) + (usd or 0)
            self._spent = (total + (usd or 0), by_stage)

    def spent(self):
        with self.lock:
            if self._spent is None:
                total, by_stage = 0.0, {}
                if os.path.exists(self.path('costs.jsonl')):
                    with open(self.path('costs.jsonl'), encoding='utf-8') as f:
                        for l in f:
                            try:
                                r = json.loads(l)
                            except ValueError:
                                continue
                            total += r.get('usd', 0) or 0
                            by_stage[r['stage']] = by_stage.get(r['stage'], 0) + (r.get('usd', 0) or 0)
                self._spent = (total, by_stage)
            return self._spent

    # -- style / glossary ----------------------------------------------
    def style(self):
        try:
            with open(self.path('style.md'), encoding='utf-8') as f:
                return f.read()
        except OSError:
            return ''

    def set_style(self, text):
        with open(self.path('style.md'), 'w', encoding='utf-8') as f:
            f.write(text or '')

    def glossary(self):
        if self._glossary is None:
            self._glossary = self._read_json('glossary.json', {'names': {}, 'terms': []})
        return self._glossary

    def set_glossary(self, g):
        self._write_json('glossary.json', g)
        self._glossary = g
