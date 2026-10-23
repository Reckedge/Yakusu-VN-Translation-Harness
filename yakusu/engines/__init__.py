"""Engine registry and detection."""
import os

from .base import EngineError, Match
from .generic import detect_unsupported, make_engine_classes
from .kirikiri import Kirikiri
from .manual import ScriptFolder

ENGINES = [Kirikiri] + make_engine_classes()
BY_ID = {e.id: e for e in ENGINES}
BY_ID['scripts'] = ScriptFolder


def detect(game_dir):
    """Return a list of Match objects, best first."""
    if not os.path.isdir(game_dir):
        raise EngineError('That folder doesn\'t exist.')
    matches = []
    for cls in ENGINES:
        try:
            m = cls.detect(game_dir)
        except OSError:
            m = None
        if m:
            matches.append(m)
    matches.sort(key=lambda m: -m.confidence)
    if not matches:
        u = detect_unsupported(game_dir)
        if u:
            matches.append(u)
    return matches


def engine_for(match_dict):
    m = Match(match_dict['engine_id'], match_dict['name'], match_dict.get('confidence', 0),
              match_dict.get('details'), match_dict.get('support', 'auto'), match_dict.get('note', ''))
    cls = BY_ID.get(m.engine_id)
    if not cls:
        raise EngineError(f'{m.name} isn\'t supported. {m.note}')
    return cls(m)
