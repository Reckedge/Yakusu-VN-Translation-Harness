"""Thin wrapper around lifegpc/msg-tool (GPL-3.0, bundled as a separate program).

https://github.com/lifegpc/msg-tool
"""
import os

from .base import EngineError, run_tool, tool_path


def exe():
    p = tool_path('msg-tool') or tool_path('msg_tool')
    if not p:
        raise EngineError('msg-tool is missing. Reinstall Yakusu (it ships inside the app).')
    return p


def export(src, out_dir, log, script_type=None, encoding=None, extra=()):
    os.makedirs(out_dir, exist_ok=True)
    args = [exe(), 'export', '-x', '2', '-T', 'json', '-r']
    if script_type:
        args += ['-t', script_type]
    if encoding:
        args += ['-e', encoding]
    args += list(extra) + [src, out_dir]
    code, out = run_tool(args, log)
    produced = any(f.endswith('.json') for _, _, fs in os.walk(out_dir) for f in fs)
    if code != 0 and not produced:
        raise EngineError(f'msg-tool could not read the scripts:\n{_tail(out)}')
    if code != 0:
        log('Some files could not be read and were left untranslated (see above).')
    return out


def import_(src, json_dir, out_dir, log, script_type=None, encoding=None, patched_encoding=None, extra=()):
    os.makedirs(out_dir, exist_ok=True)
    args = [exe(), 'import', '-x', '2', '-T', 'json', '-r']
    if script_type:
        args += ['-t', script_type]
    if encoding:
        args += ['-e', encoding]
    if patched_encoding:
        args += ['-p', patched_encoding]
    args += list(extra) + [src, json_dir, out_dir]
    code, out = run_tool(args, log)
    if code != 0:
        raise EngineError(f'msg-tool could not write the translated scripts:\n{_tail(out)}')
    return out


def unpack(archive, out_dir, log, archive_type=None, extra=()):
    os.makedirs(out_dir, exist_ok=True)
    args = [exe(), 'unpack', '-x', '2']
    if archive_type:
        args += ['-t', archive_type]
    args += list(extra) + [archive, out_dir]
    code, out = run_tool(args, log)
    if code != 0:
        raise EngineError(f'msg-tool could not open {os.path.basename(archive)}:\n{_tail(out)}')
    return out


def pack(src_dir, archive, archive_type, log, extra=()):
    os.makedirs(os.path.dirname(archive), exist_ok=True)
    args = [exe(), 'pack', '-x', '2', '-r', '-t', archive_type] + list(extra) + [src_dir, archive]
    code, out = run_tool(args, log)
    if code != 0 or not os.path.exists(archive):
        raise EngineError(f'msg-tool could not build {os.path.basename(archive)}:\n{_tail(out)}')
    return out


def _tail(out, n=12):
    lines = [l for l in (out or '').strip().splitlines() if l.strip()]
    return '\n'.join(lines[-n:]) or '(no output)'
