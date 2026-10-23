"""Engines handled by msg-tool's archive + script support.

Each profile says how to find the game's script archive, which msg-tool
archive/script types to use, and the text encoding. Flow:

  unpack archive -> export scripts -> (translate) -> import -> repack the
  archive with the patched scripts -> install over the original (backed up).

Engines where the archive can be read but not rebuilt are "manual": Yakusu
produces the patched script files and tells you where they go.
"""
import fnmatch
import os
import shutil

from . import msgtool
from .base import Engine, EngineError, Match, clean_dir, list_files

# id, display name, archive glob(s), msg-tool archive type, script type, encoding,
# support level, marker files that confirm the engine, note
PROFILES = [
    dict(id='favorite', name='FAVORITE (FVP)', loose=['*.hcb'], script_type='favorite',
         encoding='cp932', support='experimental',
         note='Replaces the .hcb script in place. Keep the backup: old saves may not load with a rebuilt script.'),
    dict(id='artemis', name='Artemis', archives=['*.pfs', '*.pfs.*'], archive_type='artemis-arc',
         encoding='utf-8', support='experimental'),
    dict(id='bgi', name='BGI / Ethornell', archives=['data0*.arc'], markers=['BGI.exe', 'sysprg.arc', '*.gdb'],
         archive_type='bgi-arc', script_type='bgi', encoding='cp932', support='experimental'),
    dict(id='yuris', name='YU-RIS', archives=['*.ypf'], archive_type='yuris-ypf', encoding='cp932',
         support='experimental'),
    dict(id='qlie', name='QLiE', archives=['GameData/*.pack', '*.pack'], archive_type='qlie-pack',
         encoding='cp932', support='experimental', pack_extra=['--backslash']),
    dict(id='circus', name='Circus', archives=['Advdata/*.dat', '*.pck'], archive_type='circus-pck',
         script_type='circus', encoding='cp932', support='experimental'),
    dict(id='escude', name='Escu:de', archives=['script.bin'], archive_type='escude-arc',
         encoding='cp932', support='experimental'),
    dict(id='catsystem2', name='CatSystem2', archives=['scene.int', '*.int'], archive_type='cat-system-int',
         script_type='cat-system', encoding='cp932', support='manual', no_repack=True,
         note='Yakusu can read CatSystem2 archives but can\'t rebuild them yet. You\'ll get patched .cst files to pack with CatSystem2 tools.'),
    dict(id='softpal', name='Softpal', archives=['data.pac'], archive_type='softpal-pac', script_type='softpal',
         encoding='cp932', support='manual', no_repack=True,
         note='Yakusu can read Softpal archives but can\'t rebuild them yet. You\'ll get patched script files.'),
]

# Engines we recognize but can't patch automatically, with what to do instead.
KNOWN_UNSUPPORTED = [
    (['Scene.pck', 'Gameexe.dat'], 'SiglusEngine',
     'SiglusEngine isn\'t supported yet. No open tool can rebuild its Scene.pck reliably.'),
    (['Rio.arc', 'Rio*.arc'], 'WillPlus / AdvHD',
     'Extract the .ws2 scripts from Rio.arc with GARbro, then use "Translate script files" and point Yakusu at that folder.'),
    (['renpy', 'game/*.rpa'], 'Ren\'Py',
     'Unpack game/*.rpa with unrpa (or use the game\'s .rpy files), then use "Translate script files" on the game/ folder.'),
    (['Seen.txt', 'Gameexe.ini'], 'RealLive',
     'Use "Translate script files" on the folder with Seen.txt (VNTextPatch handles RealLive).'),
]


def _glob(game_dir, patterns):
    hits = []
    for pat in patterns:
        parts = pat.split('/')
        base = game_dir
        for i, part in enumerate(parts):
            if i < len(parts) - 1:
                cand = [d for d in os.listdir(base) if os.path.isdir(os.path.join(base, d)) and fnmatch.fnmatch(d.lower(), part.lower())] if os.path.isdir(base) else []
                if not cand:
                    base = None
                    break
                base = os.path.join(base, cand[0])
        if not base:
            continue
        try:
            for f in os.listdir(base):
                if fnmatch.fnmatch(f.lower(), parts[-1].lower()) and os.path.isfile(os.path.join(base, f)):
                    hits.append(os.path.relpath(os.path.join(base, f), game_dir).replace('\\', '/'))
        except OSError:
            pass
    return sorted(set(hits))


class MsgToolEngine(Engine):
    profile = None

    @classmethod
    def detect_profile(cls, game_dir, prof):
        if prof.get('markers') and not _glob(game_dir, prof['markers']):
            return None
        files = _glob(game_dir, prof.get('archives') or prof.get('loose') or [])
        if not files:
            return None
        conf = 0.8 if prof.get('markers') else 0.6
        return Match(prof['id'], prof['name'], conf,
                     {'files': files, 'profile': prof['id']}, prof['support'], prof.get('note', ''))

    @property
    def prof(self):
        pid = self.details.get('profile') or self.match.engine_id
        return next(p for p in PROFILES if p['id'] == pid)

    @property
    def encoding(self):
        return self.prof['encoding']

    @property
    def straight_quotes(self):
        return self.prof['encoding'] != 'utf-8'

    def prepare(self, project, log):
        prof = self.prof
        game = project.game_dir
        src_root = project.work('scripts')
        clean_dir(src_root)
        json_root = project.work('json')
        clean_dir(json_root)
        for rel in self.details['files']:
            path = os.path.join(game, rel)
            key = rel.replace('/', '__')
            dst = os.path.join(src_root, key)
            if prof.get('archive_type'):
                log(f'Unpacking {rel}')
                msgtool.unpack(path, dst, log, prof['archive_type'])
                export_src = dst
            else:
                os.makedirs(dst, exist_ok=True)
                shutil.copy2(path, os.path.join(dst, os.path.basename(rel)))
                export_src = dst
            msgtool.export(export_src, os.path.join(json_root, key), log, prof.get('script_type'))
        if not list_files(json_root, {'.json'}):
            raise EngineError(f'No text was found in the {prof["name"]} scripts. The game may use a variant msg-tool doesn\'t read yet.')
        return json_root

    def build(self, project, log):
        prof = self.prof
        src_root = project.work('scripts')
        out = project.work('out')
        clean_dir(out)
        manual = project.work('manual')
        for rel in self.details['files']:
            key = rel.replace('/', '__')
            src = os.path.join(src_root, key)
            json_dir = os.path.join(project.work('json_tl'), key)
            if not os.path.isdir(json_dir):
                continue
            patched = os.path.join(project.work('patched'), key)
            clean_dir(patched)
            msgtool.import_(src, json_dir, patched, log, prof.get('script_type'))
            if prof.get('archive_type') and not prof.get('no_repack'):
                # Rebuild the archive: original contents with patched scripts on top.
                stage = os.path.join(project.work('repack'), key)
                clean_dir(stage)
                shutil.copytree(src, stage, dirs_exist_ok=True)
                shutil.copytree(patched, stage, dirs_exist_ok=True)
                dst = os.path.join(out, *rel.split('/'))
                msgtool.pack(stage, dst, prof['archive_type'], log, prof.get('pack_extra', ()))
                log(f'Rebuilt {rel}')
            elif prof.get('archive_type'):
                dst = os.path.join(manual, key)
                clean_dir(dst)
                shutil.copytree(patched, dst, dirs_exist_ok=True)
                log(f'Patched scripts for {rel} are in {dst}')
            else:
                for f in list_files(patched):
                    target_rel = os.path.join(os.path.dirname(rel), os.path.basename(f)) if os.path.dirname(rel) else f
                    dst = os.path.join(out, target_rel)
                    os.makedirs(os.path.dirname(dst) or out, exist_ok=True)
                    shutil.copy2(os.path.join(patched, f), dst)
        if prof.get('no_repack'):
            self.details['manual_output'] = manual
        return out


def make_engine_classes():
    classes = []
    for prof in PROFILES:
        cls = type('Engine_' + prof['id'], (MsgToolEngine,), {'id': prof['id'], 'name': prof['name']})
        cls.detect = classmethod(lambda c, game_dir, prof=prof: MsgToolEngine.detect_profile(game_dir, prof))
        classes.append(cls)
    return classes


def detect_unsupported(game_dir):
    for markers, name, note in KNOWN_UNSUPPORTED:
        if _glob(game_dir, markers) or any(os.path.isdir(os.path.join(game_dir, m)) for m in markers):
            return Match('unsupported', name, 0.5, {}, 'manual', note)
    return None
