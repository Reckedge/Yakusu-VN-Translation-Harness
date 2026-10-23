"""Test helpers: a fake Kirikiri game and a fake LLM."""
import json
import os
import re

from yakusu.core import llm
from yakusu.engines import xp3

FIRST_KS = """*start|プロローグ
[cm]
@bg storage="bg_room"
春の朝。窓の外では、桜が静かに散っていた。[l][r]
今日から新しい学校に通う。[p]
[cm]
@chara name="sakura"
【桜】
「おはよう、大雅くん！[l]今日もいい天気だね」[p]
【大雅】
「……ああ、おはよう」[p]
僕は[ruby text="ねむ"]眠い目をこすりながら、彼女の後を追った。そして駅に向かって長い坂道を歩いていった。[p]
[iscript]
f.flag = "未使用";
[endscript]
*choice
[link target=*a]一緒に行く[endlink][r]
[link target=*b]先に行く[endlink][s]
"""

SECOND_KS = """*a
@playse storage="door"
【桜】「じゃあ、一緒に行こう！」[p]
二人で歩く道は、いつもより短く感じた。[p]
"""


def make_game(root, encrypted_flag=False):
    """A tiny Kirikiri game: data.xp3 with two scenario files, plus an exe."""
    os.makedirs(root, exist_ok=True)
    src = os.path.join(root, '_src')
    os.makedirs(os.path.join(src, 'scenario'), exist_ok=True)
    os.makedirs(os.path.join(src, 'image'), exist_ok=True)
    with open(os.path.join(src, 'scenario', 'first.ks'), 'wb') as f:
        f.write(b'\xff\xfe' + FIRST_KS.encode('utf-16-le'))
    with open(os.path.join(src, 'scenario', 'second.ks'), 'wb') as f:
        f.write(SECOND_KS.encode('cp932'))
    with open(os.path.join(src, 'image', 'bg_room.png'), 'wb') as f:
        f.write(b'\x89PNG fake')
    xp3.write_archive(src, os.path.join(root, 'data.xp3'))
    import shutil
    shutil.rmtree(src)
    with open(os.path.join(root, 'game.exe'), 'wb') as f:
        f.write(b'MZ' + b'\0' * 2048)
    return root


PH = re.compile('⟦\\d+⟧')


def fake_translate_item(jp):
    """Deterministic 'translation': English words, placeholders kept in order."""
    codes = PH.findall(jp)
    words = 'the cherry blossoms were falling quietly outside the window this morning'.split()
    n = max(3, min(len(words), len(jp) // 3))
    body = ' '.join(words[:n]).capitalize()
    if codes:
        return body + ' ' + ' '.join(codes) + ' and then more.'
    return body + '.'


class FakeClient:
    calls = []

    def __init__(self, provider, api_key, model, base_url=None, timeout=180):
        self.model = model

    def complete(self, system, user, max_tokens=4096, temperature=0.3):
        FakeClient.calls.append((system[:40], user[:60]))
        usage = {'input': len(user) // 2, 'output': 200, 'cache_write': 0, 'cache_read': 0}
        if user == 'Say OK.':
            reply = 'OK'
        elif system.startswith('You are the lead translator'):
            reply = json.dumps({'summary': 'A gentle school romance.', 'style_sheet': '- Past tense narration.',
                                'names': [{'jp': '桜', 'en': 'Sakura'}, {'jp': '大雅', 'en': 'Taiga'}],
                                'terms': []})
        elif system.startswith('You give English names'):
            table = {'桜': 'Sakura', '大雅': 'Taiga'}
            reply = json.dumps([{'jp': n, 'en': table.get(n, 'Someone')} for n in user.splitlines() if n.strip()],
                               ensure_ascii=False)
        elif 'TRANSLATE' in user:
            part = user.split('TRANSLATE', 1)[1]
            items = [json.loads(l) for l in part.splitlines() if l.strip().startswith('{')]
            reply = json.dumps([{'i': it['i'], 'en': fake_translate_item(it['jp'])} for it in items], ensure_ascii=False)
        elif system.startswith('You fit English'):
            items = [json.loads(l) for l in user.splitlines() if l.strip()]
            reply = json.dumps([{'i': it['i'], 'en': ' '.join(it['en'].split()[:4]) + ' ' + ' '.join(PH.findall(it['en']))}
                                for it in items], ensure_ascii=False)
        else:  # review: change the first line of each batch
            items = [json.loads(l) for l in user.splitlines() if l.strip()]
            reply = json.dumps([{'i': items[0]['i'], 'en': items[0]['en'].replace('quietly', 'softly')}], ensure_ascii=False)
        return reply, usage, llm.cost_usd(self.model, usage)
