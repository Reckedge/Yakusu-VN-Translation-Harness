"""The API client against a local mock server: retries, errors, truncation,
and OpenAI-compatible parameter fallbacks."""
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

from yakusu.core import llm


class Mock:
    def __init__(self):
        self.queue = []      # (status, body dict, headers)
        self.requests = []

    def handler(self):
        mock = self

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_POST(self):
                n = int(self.headers.get('Content-Length') or 0)
                mock.requests.append({'path': self.path, 'headers': {k.lower(): v for k, v in self.headers.items()},
                                      'body': json.loads(self.rfile.read(n))})
                status, body, headers = mock.queue.pop(0) if mock.queue else (500, {'error': 'empty'}, {})
                data = json.dumps(body).encode()
                self.send_response(status)
                for k, v in (headers or {}).items():
                    self.send_header(k, v)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)
        return H


@pytest.fixture
def mock():
    m = Mock()
    srv = ThreadingHTTPServer(('127.0.0.1', 0), m.handler())
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    m.url = f'http://127.0.0.1:{srv.server_address[1]}'
    yield m
    srv.shutdown()


def anthropic_ok(text, stop='end_turn'):
    return (200, {'type': 'message', 'content': [{'type': 'text', 'text': text}], 'stop_reason': stop,
                  'usage': {'input_tokens': 100, 'output_tokens': 20, 'cache_read_input_tokens': 50,
                            'cache_creation_input_tokens': 0}}, {})


def client(mock, provider='anthropic', model='claude-haiku-5-5'):
    url = mock.url + ('/v1/messages' if provider == 'anthropic' else '/v1')
    c = llm.Client(provider, 'key-123', model, url)
    c.sleep = lambda s: mock.__dict__.setdefault('slept', []).append(s)
    return c


def test_anthropic_request_shape_and_cost(mock):
    mock.queue.append(anthropic_ok('[{"i": 1, "en": "Hi"}]'))
    text, usage, usd = client(mock).complete('SYS', 'USER', max_tokens=500)
    req = mock.requests[0]
    assert req['headers']['x-api-key'] == 'key-123'
    assert req['headers']['anthropic-version'] == llm.ANTHROPIC_VERSION
    assert req['body']['system'][0]['cache_control'] == {'type': 'ephemeral'}
    assert req['body']['messages'] == [{'role': 'user', 'content': 'USER'}]
    assert usage == {'input': 100, 'output': 20, 'cache_write': 0, 'cache_read': 50}
    assert usd == pytest.approx((100 * 0.10 + 20 * 0.50 + 50 * 0.01) / 1e6)


def test_rate_limit_waits_for_retry_after_then_succeeds(mock):
    mock.queue += [(429, {'error': {'message': 'slow down'}}, {'retry-after': '7'}),
                   (529, {'error': {'message': 'overloaded'}}, {}),
                   anthropic_ok('ok')]
    c = client(mock)
    text, _, _ = c.complete('s', 'u')
    assert text == 'ok'
    assert mock.slept[0] == 7


def test_bad_key_is_fatal_immediately(mock):
    mock.queue += [(401, {'error': {'message': 'invalid x-api-key'}}, {}), anthropic_ok('never')]
    with pytest.raises(llm.LLMError) as e:
        client(mock).complete('s', 'u')
    assert e.value.fatal and 'rejected' in str(e.value)
    assert len(mock.requests) == 1


def test_out_of_credit_is_explained(mock):
    mock.queue.append((400, {'error': {'message': 'Your credit balance is too low to access the API.'}}, {}))
    with pytest.raises(llm.LLMError) as e:
        client(mock).complete('s', 'u')
    assert 'out of credit' in str(e.value)


def test_truncated_reply_raises_truncated_with_usage(mock):
    mock.queue.append(anthropic_ok('[{"i": 1, "en": "Hal', stop='max_tokens'))
    with pytest.raises(llm.Truncated) as e:
        client(mock).complete('s', 'u')
    assert e.value.args[0]['output'] == 20
    assert len(mock.requests) == 1          # not retried blindly


def test_gives_up_after_repeated_server_errors(mock):
    mock.queue += [(500, {'error': {'message': 'boom'}}, {})] * 6
    with pytest.raises(llm.LLMError) as e:
        client(mock).complete('s', 'u')
    assert e.value.fatal and 'Gave up' in str(e.value)


def test_openai_adapts_parameters(mock):
    ok = (200, {'choices': [{'message': {'content': 'hello'}, 'finish_reason': 'stop'}],
                'usage': {'prompt_tokens': 10, 'completion_tokens': 2}}, {})
    # OpenAI proper: starts with max_completion_tokens and no temperature.
    mock.queue.append(ok)
    c = client(mock, 'openai', 'some-model')
    assert c.complete('s', 'u')[0] == 'hello'
    body = mock.requests[-1]['body']
    assert 'max_completion_tokens' in body and 'temperature' not in body
    assert mock.requests[-1]['headers']['authorization'] == 'Bearer key-123'
    # A server that rejects temperature gets a retry without it.
    mock.queue += [(400, {'error': {'message': "Unsupported parameter: 'temperature'"}}, {}), ok]
    c2 = client(mock, 'openrouter', 'm')
    assert c2.complete('s', 'u')[0] == 'hello'
    assert 'temperature' in mock.requests[-2]['body'] and 'temperature' not in mock.requests[-1]['body']


def test_openai_length_finish_is_truncation(mock):
    mock.queue.append((200, {'choices': [{'message': {'content': '[{'}, 'finish_reason': 'length'}], 'usage': {}}, {}))
    with pytest.raises(llm.Truncated):
        client(mock, 'local', 'm').complete('s', 'u')


def test_parse_json_array_tolerates_fences_and_thinking():
    assert llm.parse_json_array('```json\n[{"i":1}]\n```') == [{'i': 1}]
    assert llm.parse_json_array('<think>[nope]</think>Sure: [{"i":2}] done') == [{'i': 2}]
    with pytest.raises(ValueError):
        llm.parse_json_array('no array here')
