"""Local web server behind the Yakusu window.

Binds to 127.0.0.1 only, on a random port, and requires a per-launch token
on every API call, so other websites open in your browser can't talk to it.
"""
import json
import mimetypes
import os
import secrets
import subprocess
import sys
import threading
import time
import traceback
from collections import deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

from . import config, engines
from .core import llm
from .core.pipeline import QUALITY_PRESETS, BudgetReached, Cancelled, Pipeline
from .core.project import Project
from .core.protect import protect
from .engines.base import EngineError
from .engines.kirikiri import _xp3_games
from .engines.manual import ScriptFolder

WEB = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'web')
TOKEN = secrets.token_urlsafe(24)


class App:
    def __init__(self):
        self.settings = config.load_settings()
        self.project = None
        self.matches = []
        self.logs = deque(maxlen=600)
        self.job = {'running': False}
        self.cancel = threading.Event()
        self.lock = threading.Lock()
        self.dialog = None   # set by the window shell for native folder pickers
        last = self.settings.get('last_game')
        if last and os.path.isdir(last):
            try:
                self.open_game(last, quiet=True)
            except Exception:
                pass

    # ------------------------------------------------------------ logging / jobs
    def log(self, msg):
        self.logs.append({'t': time.time(), 'm': str(msg)})

    def progress(self, **kw):
        with self.lock:
            if kw.get('stage') and kw['stage'] != self.job.get('stage_name'):
                self.job['stage_name'] = kw['stage']
                self.job['stage_started'] = time.time()
            self.job.update(kw)

    def pipeline(self):
        if not self.project:
            raise EngineError('Choose a game folder first.')
        # A snapshot of the settings: changing them mid-run doesn't affect a running job.
        return Pipeline(self.project, json.loads(json.dumps(self.settings)), self.log, self.progress, self.cancel)

    def start(self, action, fn):
        with self.lock:
            if self.job.get('running'):
                raise EngineError('Something is already running. Wait for it or press Stop.')
            # Same Event object the pipeline already holds, so Stop reaches it.
            self.cancel.clear()
            self.job = {'running': True, 'action': action, 'stage': action, 'done': 0, 'total': 0,
                        'message': '', 'started': time.time(), 'error': None}

        def run():
            try:
                result = fn()
                with self.lock:
                    self.job.update(running=False, finished=time.time(), result=result)
            except Cancelled:
                self.log('Stopped. Everything translated so far is saved.')
                with self.lock:
                    self.job.update(running=False, finished=time.time(), error=None, stopped=True)
            except (EngineError, BudgetReached, llm.LLMError) as e:
                self.log(f'Error: {e}')
                with self.lock:
                    self.job.update(running=False, finished=time.time(), error=str(e))
            except Exception as e:
                self.log(traceback.format_exc())
                with self.lock:
                    self.job.update(running=False, finished=time.time(), error=f'Unexpected error: {e}')
        threading.Thread(target=run, daemon=True).start()
        return {'started': action}

    # ------------------------------------------------------------ state
    def state(self):
        s = self.settings
        keys = {k: (v[:7] + '…' + v[-4:] if len(v) > 14 else '••••') for k, v in (s.get('keys') or {}).items() if v}
        proj = None
        if self.project:
            p = self.project
            res = p.results()
            live = [l for l in p.lines() if not l.get('skip')]
            done = sum(1 for l in live if (res.get(l['id']) or {}).get('en_ph'))
            reviewed = sum(1 for l in live if l['id'] in res and res[l['id']].get('stage') in ('review', 'reviewed', 'manual', 'fit'))
            problems = sum(1 for l in live if l['id'] in res and res[l['id']].get('problems'))
            from .engines.base import Installer
            inst = Installer(p)
            spent, by_stage = p.spent()
            proj = {
                'game_dir': p.game_dir, 'name': os.path.basename(os.path.normpath(p.game_dir)),
                'engine': p.meta.get('engine'), 'stats': p.meta.get('stats'), 'box': p.meta.get('box'),
                'settings': p.meta.get('settings'), 'extracted': bool(p.meta.get('extracted_at')),
                'has_style': bool(p.style().strip()), 'translated': done, 'reviewed': reviewed,
                'problems': problems, 'total': len(live), 'built_at': p.meta.get('built_at'),
                'installed': inst.installed, 'installed_at': inst.manifest.get('installed_at'),
                'spent': round(spent, 4), 'spent_by_stage': {k: round(v, 4) for k, v in by_stage.items()},
                'files': sorted({l['file'] for l in live}),
                'test_file': p.meta.get('test_file'),
                'manual_output': ((p.meta.get('engine') or {}).get('details') or {}).get('manual_output'),
            }
        with self.lock:
            job = dict(self.job)
        return {
            'token': None,
            'settings': {**{k: v for k, v in s.items() if k != 'keys'}, 'keys': keys},
            'presets': QUALITY_PRESETS,
            'providers': {k: {kk: vv for kk, vv in v.items()} for k, v in llm.PRESETS.items()},
            'prices': llm.PRICES,
            'matches': [m.to_dict() for m in self.matches],
            'project': proj,
            'job': job,
            'logs': list(self.logs)[-200:],
            'platform': sys.platform,
            'recent': self.recent(),
        }

    def recent(self, limit=8):
        out = []
        root = config.projects_dir()
        try:
            dirs = [os.path.join(root, d) for d in os.listdir(root)]
        except OSError:
            return out
        for d in dirs:
            try:
                with open(os.path.join(d, 'project.json'), encoding='utf-8') as f:
                    meta = json.load(f)
                mtime = os.path.getmtime(os.path.join(d, 'project.json'))
            except (OSError, ValueError):
                continue
            gd = meta.get('game_dir')
            if not gd or meta.get('hidden') or (self.project and os.path.normcase(gd) == os.path.normcase(self.project.game_dir)):
                continue
            out.append({'game_dir': gd, 'name': os.path.basename(os.path.normpath(gd)),
                        'engine': (meta.get('engine') or {}).get('name'), 'lines': (meta.get('stats') or {}).get('lines'),
                        'exists': os.path.isdir(gd), 't': mtime})
        out.sort(key=lambda r: -r['t'])
        return out[:limit]

    # ------------------------------------------------------------ actions
    def open_game(self, game_dir, quiet=False):
        game_dir = os.path.abspath(game_dir.strip().strip('"'))
        self.matches = engines.detect(game_dir)
        with self.lock:
            if not self.job.get('running'):
                self.job = {'running': False}   # old errors belong to the previous game
        self.project = Project.for_game(game_dir)
        if self.project.meta.pop('hidden', None):
            self.project.save_meta()
        if not self.project.meta.get('engine') and self.matches and self.matches[0].engine_id != 'unsupported':
            self.project.meta['engine'] = self.matches[0].to_dict()
            self.project.save_meta()
        self.settings['last_game'] = game_dir
        config.save_settings(self.settings)
        if not quiet:
            if self.matches:
                m = self.matches[0]
                self.log(f'Opened {game_dir}: looks like {m.name}.')
            else:
                self.log(f'Opened {game_dir}: engine not recognized.')
        return {'matches': [m.to_dict() for m in self.matches]}

    def set_engine(self, body):
        p = self.project
        if not p:
            raise EngineError('Choose a game folder first.')
        if body.get('scripts_folder'):
            m = ScriptFolder.for_folder(body['scripts_folder'], p.game_dir)
            p.meta['engine'] = m.to_dict()
        elif body.get('engine_id'):
            m = next((m for m in self.matches if m.engine_id == body['engine_id']), None)
            if not m:
                raise EngineError('That engine isn\'t available for this folder.')
            p.meta['engine'] = m.to_dict()
        if body.get('xp3_title') is not None and p.meta.get('engine'):
            p.meta['engine'].setdefault('details', {})['xp3_title'] = body['xp3_title'] or None
        p.save_meta()
        return {'ok': True}

    def save_settings(self, body):
        s = self.settings
        for k in ('provider', 'preset', 'review', 'budget_usd', 'batch_size', 'context_lines', 'parallel'):
            if k in body:
                s[k] = body[k]
        prov = body.get('provider') or s.get('provider')
        if body.get('api_key') is not None and body.get('api_key') != '':
            s.setdefault('keys', {})[prov] = body['api_key'].strip()
        if body.get('clear_key'):
            s.setdefault('keys', {}).pop(prov, None)
        if 'base_url' in body:
            s.setdefault('base_urls', {})[prov] = body['base_url'].strip()
        if 'translate_model' in body:
            s.setdefault('translate_model', {})[prov] = body['translate_model'].strip()
        if 'review_model' in body:
            s.setdefault('review_model', {})[prov] = body['review_model'].strip()
        config.save_settings(s)
        return {'ok': True}

    def test_key(self):
        pipe = self.pipeline() if self.project else Pipeline(None, self.settings)
        tm, _ = pipe.models()
        client = pipe.client(tm)
        text, usage, usd = client.complete('Reply with the single word OK.', 'Say OK.', max_tokens=10)
        return {'ok': True, 'reply': text.strip()[:40], 'model': tm}

    def set_box(self, body):
        p = self.project
        box = dict(p.meta.get('box') or {})
        for k in ('width', 'lines'):
            if k in body:
                box[k] = max(8, min(int(body[k]), 200)) if k == 'width' else max(1, min(int(body[k]), 12))
        box['user_set'] = True
        p.meta['box'] = box
        st = p.meta.setdefault('settings', {})
        for k in ('insert_breaks', 'quotes'):
            if k in body:
                st[k] = body[k]
        if 'test_file' in body:
            p.meta['test_file'] = body['test_file']
        p.save_meta()
        return {'ok': True}

    def preview(self, n=6):
        """Translated lines rendered for the text box preview."""
        p = self.project
        if not p:
            return {'lines': []}
        pipe = self.pipeline()
        eng = pipe.engine()
        res = p.results()
        out = []
        for l in pipe.live_lines():
            r = res.get(l['id'])
            if r and r.get('en_ph'):
                text, over = pipe.render(l, r, eng)
                lb = eng.line_break_for(l['file'])
                shown = text.split(lb) if lb else [text]
                shown = [protect_display(x) for x in shown]
                names = p.glossary().get('names') or {}
                out.append({'id': l['id'], 'name': names.get(l.get('name'), l.get('name')), 'lines': shown,
                            'over': over, 'jp': protect(l['jp'], lb).text})
                if len(out) >= n * 4:
                    break
        # Prefer long lines: they show overflow problems first.
        out.sort(key=lambda x: -sum(len(s) for s in x['lines']))
        return {'lines': out[:n], 'box': p.meta.get('box')}

    def lines(self, q='', flt='all', page=0, per=60):
        p = self.project
        if not p:
            return {'rows': [], 'total': 0}
        res = p.results()
        names = p.glossary().get('names') or {}
        try:
            eng = self.pipeline().engine()
        except EngineError:
            eng = None
        rows = []
        q = (q or '').lower()
        pipe = self.pipeline()
        gloss = pipe.glossary_issues() if flt in ('glossary', 'all', 'problems') else {}
        for l in p.lines():
            r = res.get(l['id'])
            if flt == 'untranslated' and ((r and r.get('en_ph')) or l.get('skip')):
                continue
            if flt == 'problems' and not (r and r.get('problems')) and l['id'] not in gloss:
                continue
            if flt == 'skipped' and not l.get('skip'):
                continue
            if flt == 'translated' and not (r and r.get('en_ph')):
                continue
            if flt == 'glossary' and l['id'] not in gloss:
                continue
            if flt == 'reviewed' and not (r and r.get('before')):
                continue
            en = (r or {}).get('en_ph', '')
            if q and q not in l['jp'].lower() and q not in en.lower() and q not in (l.get('name') or '').lower():
                continue
            rows.append((l, r))
        total = len(rows)
        out = []
        for l, r in rows[page * per:(page + 1) * per]:   # build display rows for this page only
            out.append({'id': l['id'], 'file': l['file'], 'who': names.get(l.get('name'), l.get('name')) or '',
                        'jp': protect(l['jp'], eng.line_break_for(l['file'])).text if eng else l['jp'],
                        'en': (r or {}).get('en_ph', ''), 'stage': (r or {}).get('stage'),
                        'problems': (r or {}).get('problems'), 'skip': l.get('skip'),
                        'before': (r or {}).get('before'), 'glossary': gloss.get(l['id'])})
        return {'rows': out, 'total': total, 'page': page, 'per': per}

    def edit_line(self, body):
        p = self.project
        line = next((l for l in p.lines() if l['id'] == body['id']), None)
        if not line:
            raise EngineError('Line not found.')
        if body.get('skip') is not None:
            line['skip'] = 'skipped by you' if body['skip'] else ''
            p.set_lines(p.lines())
            return {'ok': True}
        en = (body.get('en') or '').strip()
        eng = self.pipeline().engine()
        from .core.protect import check
        if not en:
            # Clearing a line means "translate this again".
            p.add_results([{'id': line['id'], 'en_ph': '', 'stage': 'cleared', 'model': 'you', 'src': line['jp']}])
            return {'ok': True, 'problems': []}
        pr = protect(line['jp'], eng.line_break_for(line['file']))
        probs = [x for x in check(pr, en) if x != 'still has Japanese']
        p.add_results([{'id': line['id'], 'en_ph': en.strip(), 'stage': 'manual', 'model': 'you', 'src': line['jp'],
                        **({'problems': probs} if probs else {})}])
        return {'ok': True, 'problems': probs}

    def save_style(self, body):
        p = self.project
        if 'style' in body:
            p.set_style(body['style'])
        if 'glossary' in body:
            g = body['glossary']
            p.set_glossary({'names': {k: v for k, v in (g.get('names') or {}).items() if k},
                            'terms': [t for t in (g.get('terms') or []) if t.get('jp')]})
        return {'ok': True}

    def test_ids(self):
        p = self.project
        pipe = self.pipeline()
        live = pipe.live_lines()
        f = p.meta.get('test_file')
        if not f:
            counts = {}
            for l in live:
                counts[l['file']] = counts.get(l['file'], 0) + 1
            files = sorted(counts, key=lambda x: x.lower())
            f = next((x for x in files if counts[x] >= 30), files[0] if files else None)
        chosen = [l['id'] for l in live if l['file'] == f][:150]
        return chosen, f

    def run_action(self, action):
        pipe = self.pipeline()
        if action == 'extract':
            return self.start(action, pipe.extract)
        if action == 'style':
            return self.start(action, pipe.draft_style)
        if action == 'test':
            ids, f = self.test_ids()

            def test():
                pipe.translate(ids)
                pipe.build()
                pipe.install()
                self.log(f'Test patch installed: the first {len(ids)} lines of {f}. Launch the game to check them.')
            return self.start(action, test)
        if action == 'refit':
            def refit():
                pipe.build()
                pipe.install()
                self.log('Rebuilt with the new text box settings.')
            return self.start(action, refit)
        if action == 'full':
            def full():
                pipe.translate()
                if pipe.models()[1]:
                    pipe.review()
                if pipe.glossary_issues():
                    pipe.fix_glossary()
                if pipe.overflow_lines():
                    pipe.shorten()
                pipe.build()
                pipe.install()
                self.log('Done. The translation is installed. Launch the game!')
            return self.start(action, full)
        if action == 'translate':
            return self.start(action, pipe.translate)
        if action == 'review':
            return self.start(action, pipe.review)
        if action == 'fit':
            return self.start(action, pipe.shorten)
        if action == 'build':
            return self.start(action, lambda: (pipe.build(), pipe.install()))
        if action == 'restore':
            return self.start(action, pipe.restore)
        if action == 'glossary':
            return self.start(action, pipe.fix_glossary)
        raise EngineError(f'Unknown action {action}')

    def retranslate(self, ids):
        pipe = self.pipeline()
        ids = [i for i in ids if i]
        if not ids:
            raise EngineError('Pick at least one line.')
        from .engines.base import Installer
        installed = Installer(self.project).installed

        def job():
            pipe.translate(ids, redo=True)
            if pipe.models()[1]:
                pipe.review(ids)
            if installed:
                pipe.build()
                pipe.install()
            self.log(f'Retranslated {len(ids)} line{"s" if len(ids) != 1 else ""}.')
        return self.start('retranslate', job)

    def export_csv(self):
        import csv
        import io
        p = self.project
        eng = self.pipeline().engine()
        res = p.results()
        names = p.glossary().get('names') or {}
        buf = io.StringIO()
        w = csv.writer(buf)
        w.writerow(['id', 'speaker', 'japanese', 'english', 'status'])
        for l in p.lines():
            if l.get('skip'):
                continue
            r = res.get(l['id']) or {}
            w.writerow([l['id'], names.get(l.get('name'), l.get('name')) or '',
                        protect(l['jp'], eng.line_break_for(l['file'])).text, r.get('en_ph', ''), r.get('stage', '')])
        folder = os.path.join(p.root, 'exports')
        os.makedirs(folder, exist_ok=True)
        path = os.path.join(folder, time.strftime('lines-%Y%m%d-%H%M%S.csv'))
        with open(path, 'w', encoding='utf-8-sig', newline='') as f:   # BOM so Excel reads Japanese
            f.write(buf.getvalue())
        self.log(f'Exported {path}')
        try:
            self.reveal_path(folder)
        except Exception:
            pass
        return {'path': path}

    def import_csv(self, text):
        import csv
        import io
        from .core.protect import check
        p = self.project
        eng = self.pipeline().engine()
        by_id = {l['id']: l for l in p.lines()}
        res = p.results()
        rows, changed, problems, unknown = [], 0, 0, 0
        reader = csv.DictReader(io.StringIO(text.lstrip('\ufeff')))
        if not reader.fieldnames or 'id' not in reader.fieldnames or 'english' not in reader.fieldnames:
            raise EngineError('That CSV needs "id" and "english" columns. Export from Yakusu first and edit that file.')
        for row in reader:
            l = by_id.get((row.get('id') or '').strip())
            if not l:
                unknown += 1
                continue
            en = (row.get('english') or '').strip()
            if not en or en == (res.get(l['id']) or {}).get('en_ph'):
                continue
            probs = [x for x in check(protect(l['jp'], eng.line_break_for(l['file'])), en) if x != 'still has Japanese']
            problems += bool(probs)
            rows.append({'id': l['id'], 'en_ph': en, 'stage': 'manual', 'model': 'import', 'src': l['jp'],
                         'before': (res.get(l['id']) or {}).get('en_ph'), **({'problems': probs} if probs else {})})
            changed += 1
        p.add_results(rows)
        self.log(f'Imported {changed} edited lines from CSV.')
        return {'changed': changed, 'problems': problems, 'unknown': unknown}

    def launch(self):
        eng = self.pipeline().engine()
        exe = eng.find_exe(self.project.game_dir)
        if not exe:
            raise EngineError('Couldn\'t find the game\'s .exe in its folder.')
        if sys.platform == 'win32':
            os.startfile(exe)  # noqa
        else:
            subprocess.Popen([exe], cwd=self.project.game_dir)
        return {'launched': os.path.basename(exe)}

    def reveal(self, which):
        p = self.project
        path = {'game': p.game_dir, 'project': p.root,
                'manual': ((p.meta.get('engine') or {}).get('details') or {}).get('manual_output')}.get(which)
        if not path or not os.path.exists(path):
            raise EngineError('That folder doesn\'t exist yet.')
        self.reveal_path(path)
        return {'ok': True}

    def reveal_path(self, path):
        if sys.platform == 'win32':
            os.startfile(path)  # noqa
        elif sys.platform == 'darwin':
            subprocess.Popen(['open', path])
        else:
            subprocess.Popen(['xdg-open', path], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def forget(self, game_dir):
        """Hide a game from the recent list. Its translations are kept and come
        back if you open the same folder again."""
        if not game_dir:
            raise EngineError('No game given.')
        p = Project(os.path.join(config.projects_dir(), __import__('yakusu.core.project', fromlist=['slugify']).slugify(game_dir)))
        p.meta['hidden'] = True
        p.save_meta()
        return {'ok': True}

    def browse(self, title='Choose the game folder'):
        if self.dialog:
            return {'path': self.dialog(title)}
        try:
            import tkinter
            from tkinter import filedialog
            root = tkinter.Tk()
            root.withdraw()
            root.attributes('-topmost', True)
            path = filedialog.askdirectory(title=title)
            root.destroy()
            return {'path': path or ''}
        except Exception:
            return {'path': '', 'error': 'No folder picker available. Paste the folder path instead.'}


def protect_display(s):
    """Hide engine codes in the preview."""
    from .core.protect import CODE_RE
    return CODE_RE.sub('', s)


def make_handler(app):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def _send(self, code, body, ctype='application/json'):
            data = body if isinstance(body, bytes) else json.dumps(body, ensure_ascii=False).encode('utf-8')
            self.send_response(code)
            self.send_header('Content-Type', ctype)
            self.send_header('Content-Length', str(len(data)))
            self.send_header('Cache-Control', 'no-store')
            self.end_headers()
            self.wfile.write(data)

        def _authed(self, qs):
            return self.headers.get('X-Yakusu-Token') == TOKEN or (qs.get('token') or [''])[0] == TOKEN

        def do_GET(self):
            u = urlparse(self.path)
            qs = parse_qs(u.query)
            if not u.path.startswith('/api/'):
                return self._static(u.path, qs)
            if not self._authed(qs):
                return self._send(403, {'error': 'forbidden'})
            try:
                if u.path == '/api/state':
                    return self._send(200, app.state())
                if u.path == '/api/estimate':
                    if not (app.project and app.project.meta.get('stats')):
                        return self._send(200, {})
                    if (qs.get('all') or [''])[0]:
                        out = {}
                        for k in QUALITY_PRESETS:
                            pre = QUALITY_PRESETS[k]
                            st = {**app.settings, 'preset': k, 'review': bool(pre['review'])}
                            out[k] = Pipeline(app.project, st).estimate()
                        return self._send(200, out)
                    return self._send(200, app.pipeline().estimate())
                if u.path == '/api/style':
                    p = app.project
                    return self._send(200, {'style': p.style(), 'glossary': p.glossary()} if p else {})
                if u.path == '/api/preview':
                    return self._send(200, app.preview())
                if u.path == '/api/lines':
                    return self._send(200, app.lines((qs.get('q') or [''])[0], (qs.get('filter') or ['all'])[0],
                                                     int((qs.get('page') or ['0'])[0])))
                if u.path == '/api/preflight':
                    if not (app.project and app.project.meta.get('stats')):
                        return self._send(200, {'notes': [], 'glossary_issues': 0})
                    pipe = app.pipeline()
                    return self._send(200, {'notes': pipe.preflight(), 'glossary_issues': len(pipe.glossary_issues()),
                                            'overflow': len(pipe.overflow_lines())})
                if u.path == '/api/xp3_games':
                    q = (qs.get('q') or [''])[0].lower()
                    games = [g for g in _xp3_games() if q in g['name'].lower() or q in (g.get('title') or '').lower()]
                    return self._send(200, {'games': games[:60]})
                return self._send(404, {'error': 'not found'})
            except (EngineError, llm.LLMError) as e:
                return self._send(400, {'error': str(e)})
            except Exception as e:
                app.log(traceback.format_exc())
                return self._send(500, {'error': f'Unexpected error: {e}'})

        def do_POST(self):
            u = urlparse(self.path)
            qs = parse_qs(u.query)
            if not self._authed(qs):
                return self._send(403, {'error': 'forbidden'})
            try:
                n = int(self.headers.get('Content-Length') or 0)
                body = json.loads(self.rfile.read(n) or b'{}') if n else {}
                route = {
                    '/api/open': lambda: app.open_game(body['game_dir']),
                    '/api/engine': lambda: app.set_engine(body),
                    '/api/settings': lambda: app.save_settings(body),
                    '/api/test_key': app.test_key,
                    '/api/box': lambda: app.set_box(body),
                    '/api/style': lambda: app.save_style(body),
                    '/api/line': lambda: app.edit_line(body),
                    '/api/run': lambda: app.run_action(body['action']),
                    '/api/cancel': lambda: (app.cancel.set(), {'ok': True})[1],
                    '/api/launch': app.launch,
                    '/api/reveal': lambda: app.reveal(body.get('which', 'game')),
                    '/api/browse': lambda: app.browse(body.get('title') or 'Choose the game folder'),
                    '/api/retranslate': lambda: app.retranslate(body.get('ids') or []),
                    '/api/export_csv': app.export_csv,
                    '/api/import_csv': lambda: app.import_csv(body.get('text') or ''),
                    '/api/forget': lambda: app.forget(body.get('game_dir')),
                }.get(u.path)
                if not route:
                    return self._send(404, {'error': 'not found'})
                return self._send(200, route())
            except (EngineError, llm.LLMError, BudgetReached) as e:
                return self._send(400, {'error': str(e)})
            except KeyError as e:
                return self._send(400, {'error': f'Missing field {e}'})
            except Exception as e:
                app.log(traceback.format_exc())
                return self._send(500, {'error': f'Unexpected error: {e}'})

        def _static(self, path, qs):
            if path in ('/', ''):
                path = '/index.html'
            full = os.path.normpath(os.path.join(WEB, path.lstrip('/')))
            if not full.startswith(os.path.normpath(WEB)) or not os.path.isfile(full):
                return self._send(404, b'not found', 'text/plain')
            with open(full, 'rb') as f:
                data = f.read()
            if path == '/index.html':
                data = data.replace(b'__YAKUSU_TOKEN__', TOKEN.encode())
            ctype = mimetypes.guess_type(full)[0] or 'application/octet-stream'
            if ctype.startswith('text/') or ctype.endswith('javascript'):
                ctype += '; charset=utf-8'
            return self._send(200, data, ctype)

    return Handler


def serve(port=0):
    app = App()
    httpd = ThreadingHTTPServer(('127.0.0.1', port), make_handler(app))
    httpd.daemon_threads = True
    t = threading.Thread(target=httpd.serve_forever, daemon=True)
    t.start()
    return app, httpd, f'http://127.0.0.1:{httpd.server_address[1]}/'
