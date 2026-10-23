from yakusu.core import protect as P, textfit as T
from yakusu.engines import ks, xp3


def test_protect_roundtrip_keeps_codes_and_lifts_quotes():
    p = P.protect('[cm]【桜】「おはよう！[l]いい天気だね」[p]', '[r]')
    assert p.text == 'おはよう！⟦2⟧いい天気だね'
    assert p.name_prefix[1] == '桜'
    out = P.restore(p, 'Morning!⟦2⟧ Nice weather.', name='Sakura')
    assert out == '[cm]【Sakura】"Morning![l] Nice weather."[p]'


def test_check_flags_missing_and_extra_codes_and_japanese():
    p = P.protect('あ[l]い[l]う', '[r]')
    assert P.check(p, 'A ⟦1⟧ B ⟦2⟧ C') == []
    assert 'missing codes [2]' in P.check(p, 'A ⟦1⟧ B C')[0]
    assert any('unexpected' in x for x in P.check(p, 'A ⟦1⟧⟦1⟧ ⟦2⟧'))
    assert 'still has Japanese' in P.check(p, 'A ⟦1⟧ B ⟦2⟧ うん')


def test_dropped_codes_are_reappended():
    p = P.protect('あ[l]い', '[r]')
    assert P.restore(p, 'A B').endswith('[l]')


def test_layout_breaks_removed_structural_breaks_kept():
    p = P.protect('一行目\n二行目[l]\n三行目', '\n')
    assert '\n' not in p.text
    assert p.codes == ['[l]\n']


def test_ruby_is_dropped():
    assert P.protect('僕は[ruby text="ねむ"]眠い', '[r]').text == '僕は眠い'
    assert P.protect('｜漢字《かんじ》だ').text == '漢字だ'


def test_wrap_respects_width_codes_and_hard_breaks():
    text, n = T.wrap('one two three four five six seven', 10, '\n')
    assert all(len(l) <= 10 for l in text.split('\n')) and n == 4
    text, n = T.wrap('aaa[l][r]bbb ccc', 5, '[r]')
    assert text == 'aaa[l][r]bbb[r]ccc' and n == 3


def test_measure_box_explicit_and_auto():
    msgs = ['あいうえおかきくけこさしすせそ\nたちつてと'] * 30
    box = T.measure_box(msgs)
    assert box['mode'] == 'explicit' and box['width'] == 30 and box['lines'] == 2
    assert T.measure_box(['あ' * 50] * 30)['mode'] == 'auto'


def test_sjis_normalization():
    assert T.normalize_encoding('café — “hi”', 'cp932', straight_quotes=True) == 'cafe ― "hi"'


def test_ks_name_lines_and_blank_continuations():
    src = '【桜】\n「おはよう」\n「元気？」[p]\n@bg storage=x\n地の文。\n続き。[p]\n'
    units = ks.parse(src)
    assert [u['name'] for u in units] == ['桜', None]
    out = ks.apply_entries(src, [{'name': 'Sakura', 'message': '"Morning." "You okay?"[p]'},
                                 {'message': 'Narration. More.[p]'}])
    assert out.split('\n') == ['【Sakura】', '"Morning." "You okay?"[p]', '', '@bg storage=x',
                               'Narration. More.[p]', '', '']


def test_descramble_modes():
    text = '*start\n[cm]こんにちは'
    raw = bytearray(text.encode('utf-16-le'))
    m1 = bytearray(raw)
    for i in range(0, len(m1), 2):
        c = m1[i] | (m1[i + 1] << 8)
        c = ((c & 0xAAAA) >> 1) | ((c & 0x5555) << 1)
        m1[i], m1[i + 1] = c & 0xFF, c >> 8
    assert xp3.descramble(b'\xfe\xfe\x01\xff\xfe' + bytes(m1)) == text
    import struct, zlib
    z = zlib.compress(bytes(raw))
    assert xp3.descramble(b'\xfe\xfe\x02\xff\xfe' + struct.pack('<qq', len(z), len(raw)) + z) == text
    assert xp3.descramble(b'plain') is None


def test_ks_realistic_kag_patterns():
    src = '\n'.join([
        '*scene01|春',
        '\t[桜 voice=sak0001]',
        '\t「今日は[emb exp="f.name"]くんと一緒だね」[p]',
        '@nm t="大雅"',
        '「ああ」[p]',
        '[if exp="f.route==1"]',
        '[font size=30]大きな声[resetfont]で叫んだ。[p]',
        '[endif]',
        '[macro name=foo]',
        'マクロの中の文字。',
        '[endmacro]',
        '地の文が[r]',
        '続いている。[p]',
    ])
    units = ks.parse(src)
    assert [u['name'] for u in units] == ['桜', '大雅', None, None]
    assert all('マクロ' not in u['message'] for u in units)
    p = P.protect(units[0]['message'], '[r]')
    assert '[emb exp="f.name"]' in p.codes
    out = ks.apply_entries(src, [{'message': 'Today I\'m with [emb exp="f.name"]![p]', 'name': 'Sakura'},
                                 {'message': '"Yeah."[p]', 'name': 'Taiga'},
                                 {'message': '[font size=30]Loudly[resetfont] I shouted.[p]'},
                                 {'message': 'Narration goes on.[p]'}])
    lines = out.split('\n')
    assert lines[1] == '\t[桜 voice=sak0001]'          # speaker tag is never rewritten
    assert lines[3] == '@nm t="大雅"'
    assert lines[2] == '\tToday I\'m with [emb exp="f.name"]![p]'
    assert lines[5] == '[if exp="f.route==1"]' and lines[7] == '[endif]'
    assert lines[9] == 'マクロの中の文字。'
    assert lines[11] == 'Narration goes on.[p]' and lines[12] == ''


def test_ks_trailing_break_before_command_is_kept():
    src = '一行目の文章。[r]\n@wait time=100\n二行目の文章。[p]\n'
    units = ks.parse(src)
    p = P.protect(units[0]['message'], '[r]')
    assert p.codes == ['[r]']           # kept: the next text continues after the command
    assert P.restore(p, 'First line.' + p.trail.replace(p.trail, '')).endswith('[r]')
