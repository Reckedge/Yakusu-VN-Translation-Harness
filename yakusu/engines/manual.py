"""'Translate script files' mode, for engines Yakusu can't unpack itself.

You point Yakusu at a folder of already-extracted script files (from GARbro,
unrpa, or the game folder itself for Ren'Py .rpy). Yakusu tries msg-tool
first, then VNTextPatch (arcusmaximus, MIT), which covers ~25 engines,
handles Shift-JIS "tunneling" for characters SJIS can't show, and pairs
with VNTextProxy for fonts.

If the folder is inside the game, the patched files are installed back in
place (with backups). Otherwise they're written to an output folder.
"""
import os
import shutil

from . import msgtool
from .base import Engine, EngineError, Match, clean_dir, list_files, run_tool, tool_path

VNTP_FORMATS = {
    # extension -> (VNTextPatch --format, engine label)
    '.ws2': (None, 'AdvHD'), '.bin': (None, 'ArcGameEngine'), '.asb': (None, 'Artemis'), '.ast': (None, 'Artemis'),
    '.cst': (None, 'CatSystem2'), '.a0': (None, 'Cyberworks'), '.ks': (None, 'Kirikiri'), '.scn': (None, 'Kirikiri'),
    '.mjo': (None, 'Majiro'), '.sc': (None, 'Musica'), '.nut': (None, 'Mware'), '.msc': (None, 'Propeller'),
    '.rpy': (None, 'Ren\'Py'), '.hst': (None, 'ShSystem'), '.mes': (None, 'Silky\'s'), '.s': (None, 'QLiE'),
    '.nnn': (None, 'SystemNNN'), '.spt': (None, 'SystemNNN'), '.srp': (None, 'TmrHiroAdvSystem'),
    '.ybn': (None, 'YU-RIS'), '.src': (None, 'Softpal'),
}


class ScriptFolder(Engine):
    id = 'scripts'
    name = 'Script files'
    encoding = 'cp932'

    def __init__(self, match):
        super().__init__(match)
        self.folder = self.details.get('folder')

    @classmethod
    def for_folder(cls, folder, game_dir):
        exts = {}
        for rel in list_files(folder):
            e = os.path.splitext(rel)[1].lower()
            exts[e] = exts.get(e, 0) + 1
        guess = None
        for e, n in sorted(exts.items(), key=lambda kv: -kv[1]):
            if e in VNTP_FORMATS:
                guess = VNTP_FORMATS[e][1]
                break
        inside = os.path.normcase(os.path.abspath(folder)).startswith(os.path.normcase(os.path.abspath(game_dir)) + os.sep)
        return Match('scripts', f'Script files{" (" + guess + ")" if guess else ""}', 0.5,
                     {'folder': os.path.abspath(folder), 'guess': guess, 'inside_game': inside,
                      'vntp_format': None}, 'auto' if inside else 'manual')

    @property
    def straight_quotes(self):
        return self.details.get('guess') not in ('Ren\'Py', 'Kirikiri', 'Artemis')

    def prepare(self, project, log):
        if not self.folder or not os.path.isdir(self.folder):
            raise EngineError('Choose the folder that holds the extracted script files.')
        count, size = 0, 0
        for d, _, fs in os.walk(self.folder):
            for f in fs:
                count += 1
                try:
                    size += os.path.getsize(os.path.join(d, f))
                except OSError:
                    pass
        if count > 5000 or size > 400 * 1024 * 1024:
            raise EngineError(f'That folder has {count:,} files ({size / 1048576:,.0f} MB). '
                              'Pick the folder that holds only the extracted script files.')
        src = project.work('scripts')
        clean_dir(src)
        shutil.copytree(self.folder, src, dirs_exist_ok=True)
        json_dir = project.work('json')
        clean_dir(json_dir)
        backend = None
        try:
            msgtool.export(src, json_dir, log)
            if list_files(json_dir, {'.json'}):
                backend = 'msg-tool'
        except EngineError as e:
            log(f'msg-tool: {e}')
        if not backend:
            vntp = tool_path('VNTextPatch')
            if not vntp:
                raise EngineError('msg-tool couldn\'t read these files, and VNTextPatch isn\'t available '
                                  '(it only runs on Windows). Check that the folder holds only script files.')
            clean_dir(json_dir)
            args = [vntp, 'extractlocal', src, json_dir]
            if self.details.get('vntp_format'):
                args.append('--format=' + self.details['vntp_format'])
            code, out = run_tool(args, log)
            if code != 0 or not list_files(json_dir, {'.json'}):
                raise EngineError('Neither msg-tool nor VNTextPatch could read these files. '
                                  'The folder should contain only the scenario scripts.')
            backend = 'vntextpatch'
        self.details['backend'] = backend
        log(f'Read scripts with {backend}')
        return json_dir

    def build(self, project, log):
        src = project.work('scripts')
        patched = project.work('patched')
        clean_dir(patched)
        tl = project.work('json_tl')
        if self.details.get('backend') == 'vntextpatch':
            args = [tool_path('VNTextPatch'), 'insertlocal', src, tl, patched]
            if self.details.get('vntp_format'):
                args.append('--format=' + self.details['vntp_format'])
            code, out = run_tool(args, log)
            if code != 0:
                raise EngineError('VNTextPatch could not write the translated scripts. See the log for details.')
        else:
            msgtool.import_(src, tl, patched, log)
        out = project.work('out')
        clean_dir(out)
        if self.details.get('inside_game'):
            rel_base = os.path.relpath(self.folder, project.game_dir)
            for rel in list_files(patched):
                dst = os.path.join(out, rel_base, rel)
                os.makedirs(os.path.dirname(dst), exist_ok=True)
                shutil.copy2(os.path.join(patched, rel), dst)
        else:
            self.details['manual_output'] = patched
        return out
