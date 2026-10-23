"""The local server: token protection, the main flow over HTTP, and Stop."""
import json
import time
import urllib.error
import urllib.request

import pytest

from fakes import FakeClient, make_game


@pytest.fixture
def srv(tmp_path, monkeypatch):
    monkeypatch.setenv('YAKUSU_HOME', str(tmp_path / 'home'))
    from yakusu.core import llm
    monkeypatch.setattr(llm, 'Client', FakeClient)
    from yakusu import server
    app, httpd, url = server.serve(0)
    game = make_game(str(tmp_path / 'ゲーム'))
    yield app, url, server.TOKEN, game
    httpd.shutdown()


def call(url, path, token, body=None):
    req = urllib.request.Request(url.rstrip('/') + path, method='POST' if body is not None else 'GET',
                                 data=json.dumps(body).encode() if body is not None else None,
                                 headers={'X-Yakusu-Token': token, 'Content-Type': 'application/json'} if token else {})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())


def wait_job(url, token, timeout=15):
    t0 = time.time()
    while time.time() - t0 < timeout:
        st = call(url, '/api/state', token)
        if not st['job'].get('running'):
            return st
        time.sleep(0.05)
    raise AssertionError('job did not finish')


def test_api_requires_token(srv):
    app, url, token, game = srv
    with pytest.raises(urllib.error.HTTPError) as e:
        call(url, '/api/state', None)
    assert e.value.code == 403
    with pytest.raises(urllib.error.HTTPError) as e:
        call(url, '/api/state', 'wrong')
    assert e.value.code == 403
    # The page itself is served and carries the token.
    html = urllib.request.urlopen(url).read().decode()
    assert token in html


def test_flow_over_http(srv):
    app, url, token, game = srv
    r = call(url, '/api/open', token, {'game_dir': game})
    assert r['matches'][0]['engine_id'] == 'kirikiri'
    call(url, '/api/settings', token, {'provider': 'anthropic', 'api_key': 'sk-test', 'preset': 'quick'})
    st = call(url, '/api/state', token)
    assert 'sk-test' not in json.dumps(st)            # key never sent back in full
    call(url, '/api/run', token, {'action': 'extract'})
    st = wait_job(url, token)
    assert st['project']['total'] == 8 and not st['job'].get('error')
    est = call(url, '/api/estimate?all=1', token)
    assert set(est) == {'quick', 'recommended', 'best'}
    call(url, '/api/run', token, {'action': 'test'})
    st = wait_job(url, token)
    assert st['project']['installed'] and not st['job'].get('error'), st['job']
    prev = call(url, '/api/preview', token)
    assert prev['lines'] and all(isinstance(x, str) for x in prev['lines'][0]['lines'])
    lines = call(url, '/api/lines?filter=all', token)
    assert lines['total'] == 8
    lid = lines['rows'][0]['id']
    r = call(url, '/api/line', token, {'id': lid, 'en': 'Edited by hand.'})
    assert r['ok']
    call(url, '/api/run', token, {'action': 'restore'})
    st = wait_job(url, token)
    assert not st['project']['installed']


def test_second_job_is_refused_while_running(srv):
    app, url, token, game = srv
    call(url, '/api/open', token, {'game_dir': game})
    import threading
    gate = threading.Event()
    app.start('slow', lambda: gate.wait(5))
    with pytest.raises(urllib.error.HTTPError) as e:
        call(url, '/api/run', token, {'action': 'extract'})
    assert e.value.code == 400
    gate.set()
    wait_job(url, token)


def test_stop_reaches_the_running_pipeline(srv):
    app, url, token, game = srv
    call(url, '/api/open', token, {'game_dir': game})
    call(url, '/api/settings', token, {'api_key': 'k'})
    call(url, '/api/run', token, {'action': 'extract'})
    wait_job(url, token)
    pipe = app.pipeline()
    seen = {}

    def job():
        # Wait until Stop is pressed, then check the pipeline sees it.
        for _ in range(100):
            if pipe.cancel.is_set():
                seen['cancelled'] = True
                return
            time.sleep(0.02)
    app.start('probe', job)
    call(url, '/api/cancel', token, {})
    wait_job(url, token)
    assert seen.get('cancelled')
