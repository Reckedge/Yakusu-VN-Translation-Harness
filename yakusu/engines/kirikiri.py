"""Kirikiri / KiriKiri Z (KAG). The most common VN engine, fully automatic.

Out:  read the .xp3 archives (msg-tool decrypts ~500 known encrypted games),
      descramble .ks files, export text with msg-tool.
In:   import translations with msg-tool, pack only the changed scripts into a
      new patchN.xp3 next to the game. The originals are never touched.
      For encrypted games we also drop in KirikiriUnencryptedArchive's
      version.dll so the game accepts our unencrypted patch.
"""
import difflib
import json
import os
import re
import shutil

from . import ks, msgtool, xp3
from .base import Engine, EngineError, Installer, Match, clean_dir, list_files, read_json_messages, write_json_messages
from .. import config

SCRIPT_EXTS = ('.ks', '.scn', '.ks.scn')
# Archives that only ever hold images, audio or video: never worth decrypting.
MEDIA_RE = re.compile(r'^(voice|bgm|se|sound|sysse|image|bgimage|fgimage|evimage|video|movie|fg|bg|ev|chara|face|thum)\d*\.xp3$', re.I)


def _xp3_games():
    path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'data', 'xp3_games.json')
    try:
        with open(path, encoding='utf-8') as f:
            return json.load(f)
    except (OSError, ValueError):
        return []


def guess_game_title(game_dir):
    """Pick the msg-tool decryption profile for an encrypted game.

    Best signal: the game ships the .tpm plugin named in the profile.
    Otherwise fuzzy-match the folder and exe names against known titles.
    """
    games = _xp3_games()
    for g in games:
        tpm = g.get('tpm')
        if tpm and os.path.exists(os.path.join(game_dir, *tpm.replace('\\', '/').split('/'))):
            return g['name'], 'plugin'
    words = [os.path.basename(os.path.normpath(game_dir))]
    words += [os.path.splitext(f)[0] for f in os.listdir(game_dir) if f.lower().endswith('.exe')]
    best, score = None, 0.0
    for g in games:
        for label in [g['name']] + [t.strip() for t in (g.get('title') or '').split('|') if t.strip()]:
            for w in words:
                s = difflib.SequenceMatcher(None, w.lower(), label.lower()).ratio()
                if s > score:
                    best, score = g['name'], s
    if score >= 0.72:
        return best, 'name'
    return None, None


class Kirikiri(Engine):
    id = 'kirikiri'
    name = 'Kirikiri (KAG)'
    straight_quotes = False

    @classmethod
    def detect(cls, game_dir):
        archives = [f for f in os.listdir(game_dir) if f.lower().endswith('.xp3')]
        if not archives:
            return None
        details = {'archives': sorted(archives)}
        return Match(cls.id, cls.name, 0.95, details, 'auto')

    # ------------------------------------------------------------ prepare
    def _source_archives(self, project):
        added = set(Installer(project).manifest.get('added', []))
        out = []
        for f in sorted(os.listdir(project.game_dir)):
            if f.lower().endswith('.xp3') and f not in added:
                out.append(f)
        # data.xp3 / scenario archives first; patches last so they override.
        def order(name):
            n = name.lower()
            if n.startswith('patch'):
                m = re.match(r'patch(\d*)', n)
                return (2, int(m.group(1) or 1))
            return (0 if 'scen' in n or n == 'data.xp3' else 1, 0)
        return sorted(out, key=order)

    def prepare(self, project, log):
        game = project.game_dir
        scripts_dir = project.work('scripts')
        clean_dir(scripts_dir)
        title = self.details.get('xp3_title')
        encrypted = False
        found = 0
        origin = {}  # script rel path -> archive name
        for arc in self._source_archives(project):
            path = os.path.join(game, arc)
            try:
                entries = xp3.read_index(path)
            except Exception:
                entries = None
            if entries is None and MEDIA_RE.search(arc):
                log(f'Skipping {arc} (media archive)')
                continue
            names = [e.name for e in entries] if entries is not None else []
            has_scripts = any(n.lower().endswith(SCRIPT_EXTS) for n in names)
            hashed = entries is not None and names and not any('.' in n for n in names[:50])
            if entries is not None and not has_scripts and not hashed:
                continue  # images, voices, ...
            # Can we read the scripts ourselves? Then the archive isn't encrypted.
            if entries is not None and has_scripts:
                sample = next(e for e in entries if e.name.lower().endswith(SCRIPT_EXTS))
                try:
                    readable = xp3.looks_like_text(xp3.read_file(path, sample)) or sample.name.lower().endswith('.scn')
                except Exception:
                    readable = False
                if readable and sample.encrypted_flag == 0:
                    log(f'Reading {arc} ({len(entries)} files)')
                    for e in entries:
                        if e.name.lower().endswith(SCRIPT_EXTS):
                            data = xp3.read_file(path, e)
                            rel = e.name.replace('\\', '/')
                            self._save_script(scripts_dir, rel, data)
                            origin[rel] = arc
                            found += 1
                    continue
            # Encrypted (or unreadable index): let msg-tool decrypt it.
            encrypted = True
            if not title:
                title, how = guess_game_title(game)
                if title:
                    log(f'Encrypted archive. Using the decryption profile for "{title}" (matched by {how}).')
                    self.details['xp3_title'] = title
                else:
                    raise EngineError(
                        f'{arc} is encrypted and Yakusu couldn\'t tell which game this is. '
                        'Pick the game from the list on the Game step and Yakusu will read the scripts again.')
            tmp = project.work('unpack', arc)
            clean_dir(tmp)
            log(f'Decrypting {arc} with msg-tool')
            msgtool.unpack(path, tmp, log, extra=['--xp3-game-title', title])
            for rel in list_files(tmp):
                if rel.lower().endswith(SCRIPT_EXTS):
                    with open(os.path.join(tmp, rel), 'rb') as f:
                        data = f.read()
                    rel2 = rel.replace('\\', '/')
                    self._save_script(scripts_dir, rel2, data)
                    origin[rel2] = arc
                    found += 1
            shutil.rmtree(tmp, ignore_errors=True)
        if not found:
            raise EngineError('No Kirikiri scenario scripts (.ks / .scn) were found in this game\'s .xp3 archives.')
        self.details['encrypted'] = encrypted
        self.details['script_origin'] = origin
        log(f'Found {found} script files')
        json_dir = project.work('json')
        clean_dir(json_dir)
        encodings = set()
        scn = []
        for rel in list_files(scripts_dir):
            if rel.lower().endswith('.scn'):
                scn.append(rel)
                continue
            with open(os.path.join(scripts_dir, rel), 'rb') as f:
                text, enc = ks.decode(f.read())
            encodings.add(enc)
            entries = ks.export_entries(text)
            if entries:
                write_json_messages(os.path.join(json_dir, 'ks', rel + '.json'), entries)
        if scn:
            # Binary scene files (KiriKiri Z / E-mote): msg-tool handles these.
            scn_src = project.work('scripts_scn')
            clean_dir(scn_src)
            for rel in scn:
                dst = os.path.join(scn_src, rel)
                os.makedirs(os.path.dirname(dst), exist_ok=True)
                shutil.copy2(os.path.join(scripts_dir, rel), dst)
            msgtool.export(scn_src, os.path.join(json_dir, 'scn'), log, 'kirikiri-scn')
        self.details['ks_encodings'] = sorted(encodings)
        return json_dir

    @property
    def encoding(self):
        return 'cp932' if 'cp932' in (self.details.get('ks_encodings') or []) else 'utf-16'

    def line_break_for(self, rel):
        return '[r]' if rel.replace('\\', '/').startswith('ks/') else '\n'

    def _save_script(self, root, rel, data):
        text = xp3.descramble(data)
        if text is not None:
            data = b'\xff\xfe' + text.encode('utf-16-le')  # descrambled scripts work as-is
        dst = os.path.join(root, *rel.split('/'))
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        with open(dst, 'wb') as f:
            f.write(data)

    # ------------------------------------------------------------ build
    def build(self, project, log):
        scripts = project.work('scripts')
        patched = project.work('patched')
        clean_dir(patched)
        tl = project.work('json_tl')
        for jrel in list_files(os.path.join(tl, 'ks'), {'.json'}) if os.path.isdir(os.path.join(tl, 'ks')) else []:
            rel = jrel[:-5]
            with open(os.path.join(scripts, rel), 'rb') as f:
                text, enc = ks.decode(f.read())
            new = ks.apply_entries(text, read_json_messages(os.path.join(tl, 'ks', jrel)))
            dst = os.path.join(patched, rel)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            with open(dst, 'wb') as f:
                f.write(ks.encode(new, enc))
        if os.path.isdir(os.path.join(tl, 'scn')):
            msgtool.import_(project.work('scripts_scn'), os.path.join(tl, 'scn'), patched, log, 'kirikiri-scn')
        changed = []
        for rel in list_files(patched):
            if not rel.lower().endswith(SCRIPT_EXTS):
                continue
            a = os.path.join(patched, rel)
            b = os.path.join(scripts, rel)
            if not os.path.exists(b) or open(a, 'rb').read() != open(b, 'rb').read():
                changed.append(rel)
        if not changed:
            raise EngineError('No translated lines to pack yet.')
        # Stage: every changed script at its archive path and by bare name, so
        # it overrides the original whichever way the game looks it up.
        stage = project.work('patchroot')
        clean_dir(stage)
        files = []
        # Bare names only when unique: two scripts with the same file name in
        # different folders must not overwrite each other.
        base_count = {}
        for rel in list_files(scripts):
            b = os.path.basename(rel).lower()
            base_count[b] = base_count.get(b, 0) + 1
        for rel in changed:
            src = os.path.join(patched, rel)
            names = {rel.replace('\\', '/')}
            if base_count.get(os.path.basename(rel).lower(), 0) == 1:
                names.add(os.path.basename(rel))
            for name in names:
                dst = os.path.join(stage, *name.split('/'))
                os.makedirs(os.path.dirname(dst), exist_ok=True)
                shutil.copy2(src, dst)
                files.append(name)
        out = project.work('out')
        clean_dir(out)
        patch_name = self._patch_name(project)
        xp3.write_archive(stage, os.path.join(out, patch_name), sorted(set(files)))
        log(f'Built {patch_name} with {len(changed)} translated scripts')
        if self.details.get('encrypted'):
            dll = _bundled('version.dll')
            if not dll:
                raise EngineError('This game is encrypted and needs version.dll from KirikiriTools, '
                                  'which is missing from the tools folder.')
            existing = os.path.join(project.game_dir, 'version.dll')
            ours = 'version.dll' in Installer(project).manifest.get('added', [])
            if os.path.exists(existing) and not ours:
                log('Warning: the game already has a version.dll; it will be backed up and replaced.')
            shutil.copy2(dll, os.path.join(out, 'version.dll'))
        return out

    def _patch_name(self, project):
        added = Installer(project).manifest.get('added', [])
        for a in added:
            if re.fullmatch(r'patch\d*\.xp3', a, re.I):
                return a
        existing = {f.lower() for f in os.listdir(project.game_dir)}
        if 'patch.xp3' not in existing:
            return 'patch.xp3'
        n = 2
        while f'patch{n}.xp3' in existing:
            n += 1
        return f'patch{n}.xp3'


def _bundled(name):
    p = os.path.join(config.bundle_dir(), 'kirikiri', name)
    return p if os.path.isfile(p) else None
