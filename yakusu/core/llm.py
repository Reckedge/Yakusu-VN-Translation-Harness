"""LLM clients: Anthropic Messages API and any OpenAI-compatible endpoint
(OpenAI, OpenRouter, DeepSeek, Gemini's OpenAI endpoint, LM Studio, Ollama).

Standard library only, so the packaged app has no SDK dependencies.
"""
import json
import re
import time
import urllib.error
import urllib.request

ANTHROPIC_URL = 'https://api.anthropic.com/v1/messages'
ANTHROPIC_VERSION = '2023-06-01'

# USD per million tokens: (input, output, cache write 5m, cache read).
# Haiku 5.5 has a higher tier for prompts over 100K tokens; our batches stay
# far below that.
PRICES = {
    'claude-haiku-5-5': (0.10, 0.50, 0.125, 0.01),
    'claude-sonnet-5-5': (2.00, 10.00, 2.50, 0.10),
    'claude-opus-5-5': (4.00, 20.00, 5.00, 0.20),
}

PRESETS = {
    'anthropic': {'label': 'Claude (Anthropic)', 'base_url': ANTHROPIC_URL, 'kind': 'anthropic',
                  'models': ['claude-haiku-5-5', 'claude-sonnet-5-5', 'claude-opus-5-5'],
                  'key_hint': 'sk-ant-...', 'key_url': 'https://console.anthropic.com/settings/keys'},
    'openai': {'label': 'OpenAI', 'base_url': 'https://api.openai.com/v1', 'kind': 'openai',
               'models': [], 'key_hint': 'sk-...', 'key_url': 'https://platform.openai.com/api-keys'},
    'openrouter': {'label': 'OpenRouter', 'base_url': 'https://openrouter.ai/api/v1', 'kind': 'openai',
                   'models': [], 'key_hint': 'sk-or-...', 'key_url': 'https://openrouter.ai/keys'},
    'local': {'label': 'Local model (LM Studio / Ollama)', 'base_url': 'http://localhost:1234/v1', 'kind': 'openai',
              'models': [], 'key_hint': 'not needed', 'key_url': ''},
}


class LLMError(Exception):
    def __init__(self, message, fatal=False, retry_after=None):
        super().__init__(message)
        self.fatal = fatal
        self.retry_after = retry_after


class BadRequest(Exception):
    pass


class Truncated(Exception):
    """The reply hit max_tokens. Retrying the same request won't help; the
    caller should split the work into smaller pieces."""


def price_of(model):
    return PRICES.get(model)


def cost_usd(model, usage):
    p = PRICES.get(model)
    if not p:
        return 0.0
    return (usage.get('input', 0) * p[0] + usage.get('output', 0) * p[1]
            + usage.get('cache_write', 0) * p[2] + usage.get('cache_read', 0) * p[3]) / 1e6


class Client:
    def __init__(self, provider, api_key, model, base_url=None, timeout=180):
        preset = PRESETS.get(provider, PRESETS['openai'])
        self.provider = provider
        self.kind = preset['kind']
        self.base_url = (base_url or preset['base_url']).rstrip('/')
        self.api_key = (api_key or '').strip()
        self.model = model
        self.timeout = timeout
        self.max_attempts = 6
        self.sleep = time.sleep
        # OpenAI's newer models want max_completion_tokens and reject a custom
        # temperature; other OpenAI-compatible servers want the classic names.
        self._oa_tokens_key = 'max_completion_tokens' if provider == 'openai' else 'max_tokens'
        self._oa_temperature = provider != 'openai'

    # -- public ---------------------------------------------------------
    def complete(self, system, user, max_tokens=4096, temperature=0.3):
        """Returns (text, usage dict, usd)."""
        delay = 2
        last = None
        for attempt in range(self.max_attempts):
            try:
                if self.kind == 'anthropic':
                    text, usage = self._anthropic(system, user, max_tokens, temperature)
                else:
                    text, usage = self._openai(system, user, max_tokens, temperature)
                return text, usage, cost_usd(self.model, usage)
            except LLMError as e:
                if e.fatal:
                    raise
                last = e
                wait = e.retry_after if e.retry_after else delay
                self.sleep(min(max(wait, 0.5), 120))
                delay = min(delay * 2, 60)
        raise LLMError(f'Gave up after {self.max_attempts} tries: {last}', fatal=True)

    # -- providers ------------------------------------------------------
    def _post(self, url, headers, body):
        req = urllib.request.Request(url, data=json.dumps(body).encode('utf-8'), headers=headers, method='POST')
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as r:
                return json.loads(r.read().decode('utf-8'))
        except urllib.error.HTTPError as e:
            detail = ''
            try:
                detail = json.loads(e.read().decode('utf-8', 'replace'))
                detail = detail.get('error', {}).get('message') or str(detail)
            except Exception:
                pass
            if e.code in (401, 403):
                raise LLMError(f'The API key was rejected ({e.code}). Check it in the Translator step. {detail}'.strip(), fatal=True)
            if e.code == 404:
                raise LLMError(f'Model or endpoint not found: {self.model} at {self.base_url}. {detail}'.strip(), fatal=True)
            if e.code == 402 or 'credit balance' in str(detail).lower() or 'insufficient_quota' in str(detail).lower():
                raise LLMError(f'Your API account is out of credit. Add credit with your provider, then continue. {detail}'.strip(), fatal=True)
            if e.code == 400:
                raise BadRequest(str(detail or 'bad request'))
            retry_after = None
            try:
                retry_after = float(e.headers.get('retry-after')) if e.headers and e.headers.get('retry-after') else None
            except (TypeError, ValueError):
                retry_after = None
            label = {429: 'Rate limited', 529: 'The API is overloaded', 500: 'Server error', 502: 'Bad gateway',
                     503: 'Service unavailable', 504: 'Gateway timeout'}.get(e.code, f'HTTP {e.code}')
            raise LLMError(f'{label}. {detail}'.strip(), retry_after=retry_after)
        except urllib.error.URLError as e:
            if self.provider == 'local':
                raise LLMError(f'Can\'t reach the local model at {self.base_url}. Is LM Studio\'s server running?', fatal=True)
            raise LLMError(f'Network error: {e.reason}')
        except TimeoutError:
            raise LLMError('The request timed out.')

    def _anthropic(self, system, user, max_tokens, temperature):
        headers = {
            'content-type': 'application/json',
            'x-api-key': self.api_key,
            'anthropic-version': ANTHROPIC_VERSION,
        }
        body = {
            'model': self.model,
            'max_tokens': max_tokens,
            'temperature': temperature,
            # The system prompt (instructions + style sheet + glossary) is the
            # same for every batch, so it's cached.
            'system': [{'type': 'text', 'text': system, 'cache_control': {'type': 'ephemeral'}}],
            'messages': [{'role': 'user', 'content': user}],
        }
        try:
            data = self._post(self.base_url, headers, body)
        except BadRequest as e:
            raise LLMError(f'The API refused the request: {e}', fatal=True)
        if data.get('type') == 'error':
            raise LLMError(str(data.get('error')))
        text = ''.join(b.get('text', '') for b in data.get('content', []) if b.get('type') == 'text')
        u = data.get('usage', {})
        usage = {
            'input': u.get('input_tokens', 0) or 0,
            'output': u.get('output_tokens', 0) or 0,
            'cache_write': u.get('cache_creation_input_tokens', 0) or 0,
            'cache_read': u.get('cache_read_input_tokens', 0) or 0,
        }
        if data.get('stop_reason') == 'max_tokens':
            raise Truncated(usage)
        return text, usage

    def _openai(self, system, user, max_tokens, temperature):
        headers = {'content-type': 'application/json'}
        if self.api_key:
            headers['authorization'] = f'Bearer {self.api_key}'
        for _ in range(3):
            body = {
                'model': self.model,
                'messages': [{'role': 'system', 'content': system}, {'role': 'user', 'content': user}],
                self._oa_tokens_key: max_tokens,
            }
            if self._oa_temperature:
                body['temperature'] = temperature
            try:
                data = self._post(self.base_url + '/chat/completions', headers, body)
                break
            except BadRequest as e:
                msg = str(e).lower()
                # Adapt to what this server accepts, then try again.
                if 'temperature' in msg and self._oa_temperature:
                    self._oa_temperature = False
                elif 'max_completion_tokens' in msg and self._oa_tokens_key == 'max_tokens':
                    self._oa_tokens_key = 'max_completion_tokens'
                elif 'max_tokens' in msg and self._oa_tokens_key == 'max_completion_tokens':
                    self._oa_tokens_key = 'max_tokens'
                else:
                    raise LLMError(f'The API refused the request: {e}', fatal=True)
        else:
            raise LLMError('The API refused the request parameters.', fatal=True)
        try:
            choice = data['choices'][0]
            text = choice['message'].get('content') or ''
        except (KeyError, IndexError, TypeError, AttributeError):
            raise LLMError(f'Unexpected response: {str(data)[:200]}')
        u = data.get('usage') or {}
        usage = {'input': u.get('prompt_tokens', 0) or 0, 'output': u.get('completion_tokens', 0) or 0,
                 'cache_write': 0, 'cache_read': 0}
        if choice.get('finish_reason') == 'length':
            raise Truncated(usage)
        return text, usage


def parse_json_array(text):
    """Pull the JSON array out of a model reply (tolerates code fences and chatter)."""
    t = re.sub(r'<think>.*?</think>', '', text, flags=re.S).strip()
    if t.startswith('```'):
        t = t.split('\n', 1)[1] if '\n' in t else t
        t = t.rsplit('```', 1)[0]
    a, b = t.find('['), t.rfind(']')
    if a < 0 or b < a:
        raise ValueError('no JSON array in reply')
    return json.loads(t[a:b + 1])
