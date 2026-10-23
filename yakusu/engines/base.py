"""Engine adapters: how to get text out of a game and back in.

Every adapter does the same four things:

  detect(game_dir)       -> Match or None
  prepare(project, log)  -> unpack archives if needed, export scripts to JSON
                            files in work/json (msg-tool / VNTextPatch format:
                            [{"name": ..., "message": ...}, ...])
  build(project, log)    -> import translated JSON (work/json_tl) back into the
                            scripts and package them, staged in work/out
  install(project, log)  -> copy the staged files into the game folder, backing
                            up anything they replace; restore() undoes it.
"""
import json
import os
import shutil
import subprocess
import sys
import time

from .. import config


class EngineError(Exception):
    """A problem the user can act on. The message says what to do."""


class Match:
    def __init__(self, engine_id, name, confidence, details=None, support='auto', note=''):
        self.engine_id = engine_id
        self.name = name
        self.confidence = confidence
        self.details = details or {}
        self.support = support          # auto | experimental | manual
        self.note = note

    def to_dict(self):
        return {'engine_id': self.engine_id, 'name': self.name, 'confidence': self.confidence,
                'details': self.details, 'support': self.support, 'note': self.note}


# ----------------------------------------------------------------- tools

def tool_path(name):
    """Find a bundled tool (msg-tool, VNTextPatch, ...). Returns a path or None."""
    exe = name + ('.exe' if sys.platform == 'win32' and not name.endswith('.exe') else '')
    candidates = [
        os.path.join(config.bundle_dir(), exe),
        os.path.join(config.bundle_dir(), name, exe),
        os.path.join(config.app_dir(), 'tools', exe),
    ]
    env = os.environ.get('YAKUSU_' + name.upper().replace('-', '_'))
    if env:
        candidates.insert(0, env)
    for c in candidates:
        if c and os.path.isfile(c):
            return c
    found = shutil.which(name)
    return found


def run_tool(args, log=None, cwd=None, timeout=3600):
    """Run a command-line tool, streaming its output to the log."""
    if log:
        log('$ ' + ' '.join(_quote(a) for a in args))
    flags = 0
    if sys.platform == 'win32':
        flags = 0x08000000  # CREATE_NO_WINDOW
    try:
        p = subprocess.run(args, cwd=cwd, capture_output=True, timeout=timeout, creationflags=flags)
    except FileNotFoundError:
        raise EngineError(f'Missing tool: {args[0]}. Reinstall Yakusu or put the tool in the tools folder.')
    out = (p.stdout or b'').decode('utf-8', 'replace') + (p.stderr or b'').decode('utf-8', 'replace')
    if log and out.strip():
        for line in out.strip().splitlines()[-40:]:
            log('  ' + line)
    return p.returncode, out


def _quote(a):
    a = str(a)
    return f'"{a}"' if ' ' in a else a


# ----------------------------------------------------------------- files

def list_files(root, exts=None):
    out = []
    for d, _, files in os.walk(root):
        for f in files:
            if exts is None or os.path.splitext(f)[1].lower() in exts:
                out.append(os.path.relpath(os.path.join(d, f), root))
    return sorted(out, key=lambda p: p.replace('\\', '/').lower())


def read_json_messages(path):
    with open(path, encoding='utf-8-sig') as f:
        data = json.load(f)
    if not isinstance(data, list):
        raise EngineError(f'Unexpected JSON in {os.path.basename(path)}')
    return data


def write_json_messages(path, entries):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'w', encoding='utf-8') as f:
        json.dump(entries, f, ensure_ascii=False, indent=2)


def clean_dir(path):
    if os.path.isdir(path):
        shutil.rmtree(path)
    os.makedirs(path, exist_ok=True)


# ----------------------------------------------------------------- install / restore

class Installer:
    """Copies staged files into the game and remembers how to undo it.

    The manifest (in the project folder) lists every file we added and every
    original we replaced, with a backup copy of each original. Restore puts
    originals back and deletes what we added. Originals are backed up once,
    the first time they're replaced, so repeated installs never back up a
    patched file by mistake.
    """

    def __init__(self, project):
        self.project = project
        self.manifest_path = project.path('install.json')
        self.backup_dir = project.path('backup')
        try:
            with open(self.manifest_path, encoding='utf-8') as f:
                self.manifest = json.load(f)
        except (OSError, ValueError):
            self.manifest = {'replaced': [], 'added': [], 'installed_at': None}

    def _save(self):
        with open(self.manifest_path, 'w', encoding='utf-8') as f:
            json.dump(self.manifest, f, indent=2)

    @property
    def installed(self):
        return bool(self.manifest['replaced'] or self.manifest['added'])

    def put(self, src, rel):
        """Copy src into the game at relative path rel."""
        game = self.project.game_dir
        dst = os.path.join(game, rel)
        if os.path.exists(dst):
            if rel not in self.manifest['replaced'] and rel not in self.manifest['added']:
                bak = os.path.join(self.backup_dir, rel)
                os.makedirs(os.path.dirname(bak), exist_ok=True)
                shutil.copy2(dst, bak)
                self.manifest['replaced'].append(rel)
        else:
            if rel not in self.manifest['added']:
                self.manifest['added'].append(rel)
        os.makedirs(os.path.dirname(dst) or game, exist_ok=True)
        tmp = dst + '.yakusu-tmp'
        try:
            shutil.copy2(src, tmp)
            os.replace(tmp, dst)
        except PermissionError:
            if os.path.exists(tmp):
                os.remove(tmp)
            raise EngineError(f'Windows won\'t let Yakusu replace {rel}. Close the game (and anything else using '
                              'its files), then try again.')
        self.manifest['installed_at'] = time.time()
        self._save()

    def restore(self, log=None):
        game = self.project.game_dir
        try:
            for rel in list(self.manifest['added']):
                p = os.path.join(game, rel)
                if os.path.exists(p):
                    os.remove(p)
                    if log:
                        log(f'Removed {rel}')
            for rel in list(self.manifest['replaced']):
                bak = os.path.join(self.backup_dir, rel)
                if os.path.exists(bak):
                    shutil.copy2(bak, os.path.join(game, rel))
                    if log:
                        log(f'Restored {rel}')
        except PermissionError:
            raise EngineError('Windows won\'t let Yakusu change the game\'s files. Close the game, then try again.')
        self.manifest = {'replaced': [], 'added': [], 'installed_at': None}
        self._save()


# ----------------------------------------------------------------- base class

class Engine:
    id = 'base'
    name = 'Unknown'
    encoding = 'cp932'        # encoding the game's text must fit in
    line_break = '\n'         # how a line break appears in exported messages
    straight_quotes = True    # curly quotes look wide in most SJIS fonts

    def __init__(self, match):
        self.match = match
        self.details = dict(match.details or {})

    def line_break_for(self, rel):
        """How a line break is written in messages of this exported file."""
        return self.line_break

    @classmethod
    def detect(cls, game_dir):
        return None

    def prepare(self, project, log):
        raise NotImplementedError

    def build(self, project, log):
        raise NotImplementedError

    def install(self, project, log):
        inst = Installer(project)
        out = project.work('out')
        files = list_files(out)
        if not files:
            raise EngineError('Nothing to install yet. Build the patch first.')
        for rel in files:
            inst.put(os.path.join(out, rel), rel)
            log(f'Installed {rel}')
        return files

    def restore(self, project, log):
        Installer(project).restore(log)

    # The game executable to launch for testing.
    def find_exe(self, game_dir):
        exes = [f for f in os.listdir(game_dir) if f.lower().endswith('.exe')]
        skip = ('unins', 'setup', 'config', 'launcher', 'vcredist', 'dxsetup', 'install')
        exes = [e for e in exes if not any(s in e.lower() for s in skip)] or exes
        if not exes:
            return None
        exes.sort(key=lambda e: -os.path.getsize(os.path.join(game_dir, e)))
        return os.path.join(game_dir, exes[0])
