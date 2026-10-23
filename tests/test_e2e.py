"""End to end: fake Kirikiri game -> detect -> extract -> style -> translate ->
review -> fit -> build -> install -> restore, with a fake LLM and the real
msg-tool (skipped if msg-tool isn't available)."""
import hashlib
import os

import pytest

from fakes import FIRST_KS, FakeClient, make_game


@pytest.fixture
def env(tmp_path, monkeypatch):
    monkeypatch.setenv('YAKUSU_HOME', str(tmp_path / 'home'))
    from yakusu.core import llm
    monkeypatch.setattr(llm, 'Client', FakeClient)
    game = make_game(str(tmp_path / 'My Game'))
    return game


def sha(path):
    return hashlib.sha256(open(path, 'rb').read()).hexdigest()


def test_full_flow(env):
    from yakusu import engines
    from yakusu.core.pipeline import Pipeline
    from yakusu.core.project import Project
    from yakusu.engines import ks, xp3

    game = env
    before = {f: sha(os.path.join(game, f)) for f in os.listdir(game)}
    matches = engines.detect(game)
    assert matches[0].engine_id == 'kirikiri'

    p = Project.for_game(game)
    p.meta['engine'] = matches[0].to_dict()
    p.save_meta()
    logs = []
    settings = {'provider': 'anthropic', 'keys': {'anthropic': 'test'}, 'preset': 'recommended',
                'review': True, 'batch_size': 3, 'context_lines': 2, 'parallel': 2}
    pipe = Pipeline(p, settings, log=logs.append)

    stats = pipe.extract()
    live = pipe.live_lines()
    jp = [l['jp'] for l in live]
    # name lines are attached as speakers, not translated as text; script blocks skipped
    assert not any('未使用' in j for j in jp)
    assert any(l.get('name') == '桜' for l in live)
    assert stats['lines'] == len(live) == 8
    assert p.meta['box']['width'] >= 24

    est = pipe.estimate()
    assert est['total'][0] > 0

    pipe.draft_style()
    assert p.glossary()['names']['桜'] == 'Sakura'

    pipe.translate()
    res = p.results()
    assert all(l['id'] in res for l in live)
    assert not any(r.get('problems') for r in res.values())

    pipe.review()
    assert any(r['stage'] == 'review' for r in p.results().values())

    out = pipe.build()
    assert out['applied'] == 8
    patch = os.path.join(p.work('out'), 'patch.xp3')
    assert os.path.exists(patch)

    # The patch archive holds the translated scripts, readable by our reader.
    entries = {e.name: e for e in xp3.read_index(patch)}
    assert 'scenario/first.ks' in entries and 'first.ks' in entries
    assert all(e.adler == 0 for e in entries.values())
    text, enc = ks.decode(xp3.read_file(patch, entries['scenario/first.ks']))
    assert enc == 'utf-16'
    assert '【Sakura】' in text and '【Taiga】' in text
    assert '@bg storage="bg_room"' in text and '@chara name="sakura"' in text
    assert 'f.flag = "未使用";' in text                     # TJS untouched
    assert '[link target=*a]' in text and '[endlink][r]' in text
    assert 'ruby' not in text
    lines = text.split('\n')
    orig = FIRST_KS.split('\n')
    assert len(lines) == len(orig)                         # nothing moved
    for a, b in zip(lines, orig):
        if b.startswith(('@', '*', '[iscript', '[endscript', 'f.flag')):
            assert a == b
    second, enc2 = ks.decode(xp3.read_file(patch, entries['scenario/second.ks']))
    assert enc2 == 'cp932'
    assert '【Sakura】\u201c' in second

    # Install adds patch.xp3 only (game unencrypted), restore removes it.
    pipe.install()
    assert os.path.exists(os.path.join(game, 'patch.xp3'))
    for f, h in before.items():
        assert sha(os.path.join(game, f)) == h               # originals untouched
    # Re-extract after install must ignore our own patch.xp3
    pipe.extract()
    assert len(pipe.live_lines()) == 8
    pipe.build()
    pipe.install()
    assert not os.path.exists(os.path.join(game, 'patch2.xp3'))
    pipe.restore()
    assert sorted(os.listdir(game)) == sorted(before)
    assert p.spent()[0] > 0


def test_fit_pass_shortens_overflowing_lines(env):
    from yakusu import engines
    from yakusu.core.pipeline import Pipeline
    from yakusu.core.project import Project

    p = Project.for_game(env)
    p.meta['engine'] = engines.detect(env)[0].to_dict()
    settings = {'provider': 'anthropic', 'keys': {'anthropic': 'x'}, 'preset': 'quick', 'batch_size': 10}
    pipe = Pipeline(p, settings, log=lambda m: None)
    pipe.extract()
    pipe.translate()
    p.meta['box'] = {'mode': 'explicit', 'width': 24, 'lines': 2, 'user_set': True}
    p.save_meta()
    over = pipe.overflow_lines()
    assert over
    pipe.shorten(over)
    assert len(pipe.overflow_lines()) < len(over)


def test_budget_cap_stops(env):
    from yakusu import engines
    from yakusu.core.pipeline import BudgetReached, Pipeline
    from yakusu.core.project import Project

    p = Project.for_game(env)
    p.meta['engine'] = engines.detect(env)[0].to_dict()
    pipe = Pipeline(p, {'provider': 'anthropic', 'keys': {'anthropic': 'x'}, 'preset': 'best',
                        'batch_size': 2, 'budget_usd': 0.000001, 'parallel': 1}, log=lambda m: None)
    pipe.extract()
    with pytest.raises(BudgetReached):
        pipe.translate()


def test_repeated_lines_are_translated_once(tmp_path, monkeypatch):
    monkeypatch.setenv('YAKUSU_HOME', str(tmp_path / 'home'))
    from yakusu.core import llm
    monkeypatch.setattr(llm, 'Client', FakeClient)
    import fakes
    from yakusu import engines
    from yakusu.core.pipeline import Pipeline
    from yakusu.core.project import Project
    monkeypatch.setattr(fakes, 'SECOND_KS', '*a\n「……うん」[p]\n「……うん」[p]\n【桜】「……うん」[p]\n「……うん」[p]\n')
    game = fakes.make_game(str(tmp_path / 'g'))
    p = Project.for_game(game)
    p.meta['engine'] = engines.detect(game)[0].to_dict()
    pipe = Pipeline(p, {'provider': 'anthropic', 'keys': {'anthropic': 'x'}, 'preset': 'quick', 'batch_size': 50},
                    log=lambda m: None)
    pipe.extract()
    FakeClient.calls.clear()
    pipe.translate()
    res = p.results()
    rows = [res[l['id']] for l in pipe.live_lines() if 'うん' in l['jp']]
    assert len(rows) == 4 and all(r['en_ph'] for r in rows)
    assert sum(1 for r in rows if r['model'] == 'memory') == 2   # narration x3 -> 1 + 2 copies; Sakura's own line separate
