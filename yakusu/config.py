"""App-wide settings and folders."""
import json
import os
import sys

APP_NAME = 'Yakusu'


def app_dir():
    override = os.environ.get('YAKUSU_HOME')
    if override:
        base = override
    elif sys.platform == 'win32':
        base = os.path.join(os.environ.get('LOCALAPPDATA') or os.path.expanduser('~'), APP_NAME)
    else:
        base = os.path.join(os.path.expanduser('~'), '.yakusu')
    os.makedirs(base, exist_ok=True)
    return base


def projects_dir():
    p = os.path.join(app_dir(), 'projects')
    os.makedirs(p, exist_ok=True)
    return p


def bundle_dir():
    """Folder holding bundled third-party tools (next to the exe when frozen)."""
    if getattr(sys, 'frozen', False):
        return os.path.join(getattr(sys, '_MEIPASS', os.path.dirname(sys.executable)), 'tools')
    here = os.path.dirname(os.path.abspath(__file__))
    return os.path.join(os.path.dirname(here), 'tools')


DEFAULT_SETTINGS = {
    'provider': 'anthropic',
    'keys': {},                 # provider -> api key
    'base_urls': {},            # provider -> custom base URL
    'preset': 'recommended',    # quick | recommended | best | custom
    'translate_model': {},      # provider -> model
    'review_model': {},
    'review': True,
    'budget_usd': 0,            # 0 = no cap
    'batch_size': 40,
    'context_lines': 10,
    'parallel': 4,
}


def settings_path():
    return os.path.join(app_dir(), 'settings.json')


def load_settings():
    s = dict(DEFAULT_SETTINGS)
    try:
        with open(settings_path(), encoding='utf-8') as f:
            s.update(json.load(f))
    except (OSError, ValueError):
        pass
    return s


def save_settings(s):
    tmp = settings_path() + '.tmp'
    with open(tmp, 'w', encoding='utf-8') as f:
        json.dump(s, f, indent=2)
    os.replace(tmp, settings_path())
