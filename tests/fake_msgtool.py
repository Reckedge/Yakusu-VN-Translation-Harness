#!/usr/bin/env python3
"""A stand-in for msg-tool's command line, for testing Yakusu's plumbing.

Script format: UTF-8 text, one message per line, "name|message" or "|message".
Archive format: a zip file.
"""
import json
import os
import sys
import zipfile


def files(root):
    for d, _, fs in os.walk(root):
        for f in sorted(fs):
            yield os.path.relpath(os.path.join(d, f), root)


def main(argv):
    # drop option values
    clean, skip = [], False
    for a in argv:
        if skip:
            skip = False
            continue
        if a in ('-x', '-T', '-t', '-e', '-p', '--xp3-game-title'):
            skip = True
            continue
        if a.startswith('-'):
            continue
        clean.append(a)
    cmd, rest = clean[0], clean[1:]
    if cmd == 'export':
        src, out = rest
        n = 0
        for rel in files(src):
            if not rel.endswith('.scr'):
                continue
            entries = []
            for line in open(os.path.join(src, rel), encoding='utf-8').read().splitlines():
                name, _, msg = line.partition('|')
                e = {'message': msg.replace('\\n', '\n')}
                if name:
                    e['name'] = name
                entries.append(e)
            dst = os.path.join(out, os.path.splitext(rel)[0] + '.json')
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            json.dump(entries, open(dst, 'w', encoding='utf-8'), ensure_ascii=False)
            n += 1
        print(f'OK: {n}')
        return 0 if n else 2
    if cmd == 'import':
        src, jdir, out = rest
        for rel in files(src):
            if not rel.endswith('.scr'):
                continue
            j = os.path.join(jdir, os.path.splitext(rel)[0] + '.json')
            if not os.path.exists(j):
                continue
            entries = json.load(open(j, encoding='utf-8'))
            dst = os.path.join(out, rel)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            with open(dst, 'w', encoding='utf-8') as f:
                for e in entries:
                    f.write((e.get('name') or '') + '|' + e['message'].replace('\n', '\\n') + '\n')
        return 0
    if cmd == 'unpack':
        arc, out = rest
        zipfile.ZipFile(arc).extractall(out)
        return 0
    if cmd == 'pack':
        src, arc = rest
        with zipfile.ZipFile(arc, 'w') as z:
            for rel in files(src):
                z.write(os.path.join(src, rel), rel)
        return 0
    print('unknown command', cmd)
    return 1


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
