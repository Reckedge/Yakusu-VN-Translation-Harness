"""Generic msg-tool engines and script-folder mode, driven through a fake
msg-tool, plus pipeline behaviour under failures (truncation, cancel)."""
import hashlib
import os
import stat
import sys
import threading
import zipfile

import pytest

from fakes import FakeClient, make_game

HERE = os.path.dirname(os.path.abspath(__file__))


@pytest.fixture
def fake_tool(tmp_path, monkeypatch):
    tool = tmp_path / 'bin' / 'msg-tool'
    tool.parent.mkdir()
    tool.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{os.path.join(HERE, "fake_msgtool.py")}" "$@"\n')
    tool.chmod(tool.stat().st_mode | stat.S_IEXEC)
    monkeypatch.setenv('YAKUSU_MSG_TOOL', str(tool))
    monkeypatch.setenv('YAKUSU_HOME', str(tmp_path / 'home'))
    from yakusu.core import llm
    monkeypatch.setattr(llm, 'Client', FakeClient)
    return tool


SCRIPT = '桜|「おはよう、大雅くん！」\n|春の朝。窓の外では、桜が散っていた。\\n今日から新しい学校だ。\n|bg_room.png\n大雅|「ああ、おはよう」\n'


def sha(p):
    return hashlib.sha256(open(p, 'rb').read()).hexdigest()


def settings(**kw):
    s = {'provider': 'anthropic', 'keys': {'anthropic': 'k'}, 'preset': 'quick', 'batch_size': 10, 'parallel': 2}
    s.update(kw)
    return s


def test_archive_engine_roundtrip(tmp_path, fake_tool):
    from yakusu import engines
    from yakusu.core.pipeline import Pipeline
    from yakusu.core.project import Project

    game = tmp_path / 'YurisGame'
    game.mkdir()
    with zipfile.ZipFile(game / 'ysbin.ypf', 'w') as z:
        z.writestr('ysbin/yst00001.scr', SCRIPT)
        z.writestr('ysbin/image.bin', b'\x00\x01')
    (game / 'game.exe').write_bytes(b'MZ')
    orig = sha(game / 'ysbin.ypf')

    m = engines.detect(str(game))
    assert m[0].engine_id == 'yuris' and m[0].support == 'experimental'
    p = Project.for_game(str(game))
    p.meta['engine'] = m[0].to_dict()
    pipe = Pipeline(p, settings(), log=lambda x: None)
    st = pipe.extract()
    assert st['lines'] == 3 and st['skipped'] == 1            # the .png line is skipped
    pipe.translate()
    pipe.build()
    pipe.install()
    with zipfile.ZipFile(game / 'ysbin.ypf') as z:
        assert z.read('ysbin/image.bin') == b'\x00\x01'       # untouched content survives the repack
        text = z.read('ysbin/yst00001.scr').decode('utf-8')
    assert '桜|' not in text and 'おはよう' not in text
    assert '|bg_room.png' in text                              # skipped line left alone
    assert sha(os.path.join(p.path('backup'), 'ysbin.ypf')) == orig
    pipe.restore()
    assert sha(game / 'ysbin.ypf') == orig


def test_names_translated_in_name_field(tmp_path, fake_tool):
    from yakusu import engines
    from yakusu.core.pipeline import Pipeline
    from yakusu.core.project import Project

    game = tmp_path / 'G2'
    game.mkdir()
    with zipfile.ZipFile(game / 'ysbin.ypf', 'w') as z:
        z.writestr('a.scr', SCRIPT)
    p = Project.for_game(str(game))
    p.meta['engine'] = engines.detect(str(game))[0].to_dict()
    pipe = Pipeline(p, settings(), log=lambda x: None)
    pipe.extract()
    pipe.draft_style()
    pipe.translate()
    pipe.build()
    import json
    out = json.load(open(os.path.join(p.work('json_tl'), 'ysbin.ypf', 'a.json'), encoding='utf-8'))
    assert out[0]['name'] == 'Sakura' and out[3]['name'] == 'Taiga'
    assert out[1]['message'].count('\n') >= 0 and 'Japanese' not in out[1]['message']


def test_script_folder_mode_inside_game(tmp_path, fake_tool):
    from yakusu.core.pipeline import Pipeline
    from yakusu.core.project import Project
    from yakusu.engines.manual import ScriptFolder

    game = tmp_path / 'RenGame'
    (game / 'game').mkdir(parents=True)
    (game / 'game' / 'script.scr').write_text(SCRIPT, encoding='utf-8')
    orig = sha(game / 'game' / 'script.scr')
    p = Project.for_game(str(game))
    m = ScriptFolder.for_folder(str(game / 'game'), str(game))
    assert m.details['inside_game'] and m.support == 'auto'
    p.meta['engine'] = m.to_dict()
    pipe = Pipeline(p, settings(), log=lambda x: None)
    pipe.extract()
    pipe.translate()
    pipe.build()
    pipe.install()
    assert 'おはよう' not in (game / 'game' / 'script.scr').read_text(encoding='utf-8')
    pipe.restore()
    assert sha(game / 'game' / 'script.scr') == orig


def test_script_folder_outside_game_is_manual(tmp_path, fake_tool):
    from yakusu.core.pipeline import Pipeline
    from yakusu.core.project import Project
    from yakusu.engines.base import EngineError
    from yakusu.engines.manual import ScriptFolder

    game = tmp_path / 'G3'
    game.mkdir()
    ext = tmp_path / 'extracted'
    ext.mkdir()
    (ext / 's.scr').write_text(SCRIPT, encoding='utf-8')
    p = Project.for_game(str(game))
    p.meta['engine'] = ScriptFolder.for_folder(str(ext), str(game)).to_dict()
    pipe = Pipeline(p, settings(), log=lambda x: None)
    pipe.extract()
    pipe.translate()
    res = pipe.build()
    assert res['manual_output'] and os.path.exists(os.path.join(res['manual_output'], 's.scr'))
    with pytest.raises(EngineError):
        pipe.install()


class TruncatingClient(FakeClient):
    """Cuts off any reply covering more than 2 lines."""
    def complete(self, system, user, max_tokens=4096, temperature=0.3):
        from yakusu.core import llm
        if 'TRANSLATE' in user and user.split('TRANSLATE', 1)[1].count('"i"') > 2:
            raise llm.Truncated({'input': 10, 'output': 10})
        return super().complete(system, user, max_tokens, temperature)


def test_truncated_batches_are_split_until_they_fit(tmp_path, monkeypatch):
    monkeypatch.setenv('YAKUSU_HOME', str(tmp_path / 'home'))
    from yakusu import engines
    from yakusu.core import llm
    from yakusu.core.pipeline import Pipeline
    from yakusu.core.project import Project
    monkeypatch.setattr(llm, 'Client', TruncatingClient)
    game = make_game(str(tmp_path / 'g'))
    p = Project.for_game(game)
    p.meta['engine'] = engines.detect(game)[0].to_dict()
    pipe = Pipeline(p, settings(batch_size=8, parallel=1), log=lambda x: None)
    pipe.extract()
    pipe.translate()
    res = p.results()
    assert all(l['id'] in res and not res[l['id']].get('problems') for l in pipe.live_lines())
    assert p.spent()[0] > 0   # truncated calls were still counted


class SlowClient(FakeClient):
    first_done = threading.Event()
    release = threading.Event()

    def complete(self, system, user, *a, **kw):
        out = super().complete(system, user, *a, **kw)
        if 'TRANSLATE' in user:
            SlowClient.first_done.set()
            SlowClient.release.wait(5)
        return out


def test_cancel_stops_and_keeps_finished_work(tmp_path, monkeypatch):
    monkeypatch.setenv('YAKUSU_HOME', str(tmp_path / 'home'))
    from yakusu import engines
    from yakusu.core import llm
    from yakusu.core.pipeline import Cancelled, Pipeline
    from yakusu.core.project import Project
    monkeypatch.setattr(llm, 'Client', SlowClient)
    game = make_game(str(tmp_path / 'g'))
    p = Project.for_game(game)
    p.meta['engine'] = engines.detect(game)[0].to_dict()
    cancel = threading.Event()
    pipe = Pipeline(p, settings(batch_size=1, parallel=1), log=lambda x: None, cancel=cancel)
    pipe.extract()
    err = []

    def run():
        try:
            pipe.translate()
        except Cancelled as e:
            err.append(e)
    SlowClient.first_done.clear()
    SlowClient.release.clear()
    t = threading.Thread(target=run)
    t.start()
    assert SlowClient.first_done.wait(5)
    cancel.set()                   # stop while the first batch is in flight
    SlowClient.release.set()
    t.join(10)
    assert err, 'translate() should raise Cancelled'
    assert len(p.results()) == 1   # the batch in flight is kept, nothing after it runs
