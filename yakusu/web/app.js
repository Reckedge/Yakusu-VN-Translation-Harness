/* Yakusu UI. Talks to the local server; all state lives there. */
(function () {
  'use strict';
  const TOKEN = document.querySelector('meta[name="yakusu-token"]').content;
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));
  let S = null;              // last /api/state
  let view = 'game';
  let previewLines = [];
  let previewIdx = 0;
  let previewBox = null;
  let linesState = { filter: 'all', q: '', page: 0 };
  let estimates = null;

  // ------------------------------------------------------------ api
  async function api(path, body) {
    const opt = { headers: { 'X-Yakusu-Token': TOKEN } };
    if (body !== undefined) {
      opt.method = 'POST';
      opt.headers['Content-Type'] = 'application/json';
      opt.body = JSON.stringify(body);
    }
    const r = await fetch(path, opt);
    let data = {};
    try { data = await r.json(); } catch (e) { /* empty */ }
    if (!r.ok) throw new Error(data.error || `Request failed (${r.status})`);
    return data;
  }

  function h(tag, attrs, ...kids) {
    const el = document.createElement(tag);
    if (attrs) for (const [k, v] of Object.entries(attrs)) {
      if (v == null || v === false) continue;
      if (k === 'class') el.className = v;
      else if (k === 'text') el.textContent = v;
      else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
      else el.setAttribute(k, v === true ? '' : v);
    }
    for (const c of kids.flat(Infinity)) if (c != null && c !== false) el.append(c.nodeType ? c : document.createTextNode(String(c)));
    return el;
  }

  let toastTimer;
  function toast(msg, err) {
    const t = $('#toast');
    t.textContent = msg;
    t.className = 'toast show' + (err ? ' err' : '');
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (t.className = 'toast'), err ? 7000 : 3500);
  }
  async function safe(fn) {
    try { return await fn(); } catch (e) { toast(e.message, true); }
  }

  const money = (x) => x == null ? '—' : (x < 0.01 && x > 0 ? '<$0.01' : '$' + x.toFixed(x < 10 ? 2 : 0));
  const range = (r) => !r ? null : (r[1] < 0.01 ? 'under $0.01' : r[0] < 0.01 ? `up to ${money(r[1])}` : `about ${money(r[0])}–${money(r[1])}`);
  const num = (n) => (n || 0).toLocaleString();
  const cap = (t) => t ? t[0].toUpperCase() + t.slice(1) : t;

  // ------------------------------------------------------------ navigation
  function show(v) {
    view = v;
    $$('.view').forEach((s) => s.classList.toggle('active', s.dataset.view === v));
    $$('.rail [data-view]').forEach((b) => { if (b.dataset.view !== v) b.removeAttribute('aria-current'); else b.setAttribute('aria-current', 'page'); });
    if (S) render();
    if (v === 'translator') loadEstimates();
    if (v === 'style') loadStyle();
    if (v === 'test') loadPreview();
    if (v === 'run') loadEstimates();
    if (v === 'lines') loadLines();
    if (v === 'activity') renderLog();
  }
  $$('.rail [data-view]').forEach((b) => b.addEventListener('click', () => show(b.dataset.view)));

  // ------------------------------------------------------------ polling
  let lastFinished = undefined;
  async function refresh() {
    try { S = await api('/api/state'); } catch (e) { return; }
    render();
    const fin = S.job && S.job.finished;
    if (lastFinished !== undefined && fin && fin !== lastFinished) onJobDone();
    lastFinished = fin || null;
  }
  function loop() { refresh().finally(() => setTimeout(loop, (S && S.job && S.job.running) ? 900 : 3000)); }

  function onJobDone() {
    const j = S.job;
    if (j.error) toast(j.error, true);
    else if (j.stopped) toast('Stopped. Progress is saved.');
    else toast(({ extract: 'Scripts read.', style: 'Style sheet drafted.', test: 'Test patch installed. Launch the game to check it.',
      refit: 'Applied to the game.', full: 'Translation installed.', restore: 'Original game restored.',
      build: 'Patch rebuilt and installed.', glossary: 'Name spellings fixed.', retranslate: 'Lines retranslated.' })[j.action] || 'Done.');
    if (view === 'style') loadStyle();
    if (view === 'test') loadPreview();
    if (view === 'run' || view === 'translator') loadEstimates();
    if (view === 'lines') loadLines();
    if (view === 'game') renderGame();
  }

  // ------------------------------------------------------------ render
  function render() {
    const p = S.project;
    $('#railGame').textContent = p ? p.name : 'No game open';
    const done = {
      game: p && p.extracted,
      translator: S.settings.keys && (S.settings.keys[S.settings.provider] || S.settings.provider === 'local'),
      style: p && p.has_style,
      test: p && p.translated > 0,
      run: p && p.total && p.translated >= p.total,
    };
    $$('.steps button').forEach((b) => b.classList.toggle('done', !!done[b.dataset.view]));
    const foot = $('#railFoot');
    foot.replaceChildren();
    if (p) {
      foot.append(h('div', null, h('b', null, num(p.translated)), ` of ${num(p.total)} lines`));
      foot.append(h('div', null, h('b', null, money(p.spent)), ' spent'));
      if (p.installed) foot.append(h('div', null, 'Patch installed'));
    }
    renderJob();
    renderGame();
    renderTranslator();
    renderTest();
    renderRun();
    if (view === 'activity') renderLog();
  }

  function renderJob() {
    const j = S.job || {};
    const bar = $('#jobbar');
    bar.hidden = !j.running;
    renderBanner();
    if (!j.running) { document.title = 'Yakusu'; return; }
    const names = { extract: 'Reading scripts', style: 'Style sheet', translate: 'Translating', review: 'Reviewing',
      fit: 'Fitting text', build: 'Building', test: 'Test patch', full: 'Translating', refit: 'Building', restore: 'Restoring',
      glossary: 'Fixing names', names: 'Naming speakers', retranslate: 'Retranslating' };
    $('#jobStage').textContent = names[j.stage] || names[j.action] || 'Working';
    $('#jobMsg').textContent = j.message || '';
    const fill = $('#jobFill');
    if (j.total) {
      const pctDone = Math.round((100 * j.done) / j.total);
      fill.classList.remove('indeterminate');
      fill.style.width = pctDone + '%';
      let eta = '';
      const elapsed = Date.now() / 1000 - (j.stage_started || j.started);
      if (j.done > 0 && j.done < j.total && elapsed > 8) {
        const left = (elapsed / j.done) * (j.total - j.done);
        eta = left < 90 ? `  ·  about ${Math.max(1, Math.round(left / 10) * 10)} s left` : `  ·  about ${duration(left)} left`;
      }
      $('#jobNum').textContent = `${num(j.done)} / ${num(j.total)}` + (j.spent != null ? `  ·  ${money(j.spent)}` : '') + eta;
      document.title = `${pctDone}% · Yakusu`;
    } else {
      fill.classList.add('indeterminate');
      fill.style.width = '';
      $('#jobNum').textContent = '';
      document.title = 'Working… · Yakusu';
    }
  }
  function duration(sec) {
    const m = Math.round(sec / 60);
    if (m < 60) return `${m} min`;
    const hrs = Math.floor(m / 60);
    return `${hrs} h ${m % 60} min`;
  }
  $('#jobStop').addEventListener('click', () => safe(() => api('/api/cancel', {})));

  // A failed job leaves a banner until dismissed, with a way to try again.
  let bannerDismissed = null;
  const RETRYABLE = ['extract', 'style', 'test', 'refit', 'full', 'translate', 'review', 'fit', 'build', 'restore', 'glossary'];
  function renderBanner() {
    const j = S.job || {};
    const show = !j.running && j.error && bannerDismissed !== j.finished;
    $('#banner').hidden = !show;
    if (!show) return;
    $('#bannerTitle').textContent = ({ extract: 'Couldn\'t read the scripts', style: 'Couldn\'t draft the style sheet',
      test: 'The test patch didn\'t finish', full: 'Translation stopped', translate: 'Translation stopped', review: 'Review stopped',
      build: 'Couldn\'t build the patch', refit: 'Couldn\'t apply the changes', restore: 'Couldn\'t restore the game' })[j.action] || 'Something went wrong';
    $('#bannerMsg').textContent = j.error;
    $('#bannerRetry').hidden = !RETRYABLE.includes(j.action);
  }
  $('#bannerClose').addEventListener('click', () => { bannerDismissed = S.job.finished; renderBanner(); });
  $('#bannerLog').addEventListener('click', () => show('activity'));
  $('#bannerRetry').addEventListener('click', () => { bannerDismissed = S.job.finished; run(S.job.action); });

  // ------------------------------------------------------------ 1. game
  $('#browseGame').addEventListener('click', () => safe(async () => {
    const r = await api('/api/browse', { title: 'Choose the game folder' });
    if (r.error) toast(r.error, true);
    if (r.path) await openGame(r.path);
  }));
  $('#pathForm').addEventListener('submit', (e) => {
    e.preventDefault();
    const v = $('#gamePath').value.trim();
    if (v) safe(() => openGame(v));
  });
  async function openGame(path) {
    await api('/api/open', { game_dir: path });
    await refresh();
    show('game');
  }
  const picker = $('#picker');
  ['dragenter', 'dragover'].forEach((ev) => picker.addEventListener(ev, (e) => { e.preventDefault(); picker.classList.add('drag'); }));
  ['dragleave', 'drop'].forEach((ev) => picker.addEventListener(ev, () => picker.classList.remove('drag')));
  picker.addEventListener('drop', (e) => {
    e.preventDefault();
    const f = e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files[0];
    const path = f && (f.pywebviewFullPath || f.path);   // set by the app window
    if (path) safe(() => openGame(path));
    else if (window.pywebview) setTimeout(() => refresh().then(() => show('game')), 700);  // the app window opens it
    else toast('Your browser doesn\'t share folder paths. Paste the path instead, or use Choose game folder.', true);
  });
  // Pasting a path anywhere on the Game step opens it.
  document.addEventListener('paste', (e) => {
    if (view !== 'game' || /^(INPUT|TEXTAREA)$/.test((document.activeElement || {}).tagName || '')) return;
    const t = (e.clipboardData.getData('text') || '').trim().replace(/^"|"$/g, '');
    if (/^([A-Za-z]:[\\/]|\/|\\\\)/.test(t)) { $('#gamePath').value = t; safe(() => openGame(t)); }
  });

  function renderRecent() {
    const box = $('#recent');
    const items = (S.recent || []);
    if (!items.length) { box.replaceChildren(); return; }
    box.replaceChildren(h('div', { class: 'recent' }, h('h2', { text: S.project ? 'Other games' : 'Recent games' }),
      h('ul', null, items.map((r) => h('li', { class: r.exists ? '' : 'missing' },
        h('div', { style: 'min-width:0' }, h('div', { class: 'r-name', text: r.name }), h('div', { class: 'r-path', text: r.game_dir, title: r.game_dir })),
        h('span', { class: 'r-meta', text: r.exists ? [r.engine, r.lines ? `${num(r.lines)} lines` : null].filter(Boolean).join(', ') : 'Folder not found' }),
        h('div', { class: 'inline' },
          r.exists ? h('button', { class: 'btn', onclick: () => safe(() => openGame(r.game_dir)) }, 'Open') : null,
          h('button', { class: 'icon-x', title: 'Remove from this list (translations are kept)', 'aria-label': `Remove ${r.name} from the list`,
            onclick: () => safe(async () => { await api('/api/forget', { game_dir: r.game_dir }); refresh(); }) }, '×')))))));
  }

  function supportBadge(s) {
    const t = { auto: 'Automatic', experimental: 'Experimental', manual: 'Needs a manual step' }[s] || s;
    return h('span', { class: 'badge ' + s, text: t });
  }

  function renderGame() {
    const box = $('#detectResult');
    const p = S.project;
    renderRecent();
    if (!p) { box.replaceChildren(); return; }
    if (isTyping(box)) return;
    const eng = p.engine;
    const card = h('div', { class: 'detect' });
    const head = h('div', { class: 'detect-head' },
      h('div', null, h('h2', { text: eng ? eng.name : 'Engine not recognized' }), h('div', { class: 'path', text: p.game_dir })),
      eng ? supportBadge(eng.support) : null);
    card.append(head);
    if (eng && eng.note) card.append(h('p', { class: 'note', text: eng.note }));
    if (!eng) {
      const u = S.matches.find((m) => m.engine_id === 'unsupported');
      card.append(h('p', { class: 'note', text: u ? `${u.name}: ${u.note}` :
        'Yakusu didn\'t recognize this game\'s engine. If you have its script files extracted (for example with GARbro), translate them directly.' }));
    }
    // other matches
    const others = S.matches.filter((m) => eng && m.engine_id !== eng.engine_id && m.engine_id !== 'unsupported');
    if (others.length) {
      card.append(h('div', { class: 'alt-list' }, h('span', { class: 'hint', text: 'Also possible:' }),
        others.map((m) => h('button', { class: 'btn btn-quiet', onclick: () => safe(async () => { await api('/api/engine', { engine_id: m.engine_id }); refresh(); }) }, `Use ${m.name}`))));
    }
    // encrypted Kirikiri: title picker
    const det = (eng && eng.details) || {};
    if (eng && eng.engine_id === 'kirikiri' && (S.job.error || '').includes('encrypted') && !det.xp3_title) {
      card.append(titlePicker());
    } else if (eng && eng.engine_id === 'kirikiri' && det.xp3_title) {
      card.append(h('p', { class: 'note' }, `Encrypted archives, decrypted with the profile for “${det.xp3_title}”. `,
        h('button', { class: 'btn btn-quiet', onclick: () => safe(async () => { await api('/api/engine', { xp3_title: '' }); refresh(); }) }, 'Change')));
    }
    const actions = h('div', { class: 'row-actions' });
    if (eng && eng.engine_id !== 'unsupported') {
      actions.append(h('button', { class: 'btn btn-primary', onclick: () => run('extract') }, p.extracted ? 'Read the scripts again' : 'Read the scripts'));
    }
    actions.append(h('button', { class: 'btn btn-quiet', onclick: () => safe(async () => {
      const r = await api('/api/browse', { title: 'Choose the folder with the extracted script files' });
      if (r.path) { await api('/api/engine', { scripts_folder: r.path }); refresh(); }
    }) }, 'Translate script files instead…'));
    card.append(actions);
    if (p.stats) {
      const st = p.stats;
      card.append(h('div', { class: 'stats' },
        stat(num(st.lines), 'lines to translate'),
        stat(num(st.files), 'script files'),
        stat(num(st.jp_chars), 'Japanese characters'),
        stat(p.box ? `${p.box.width} × ${p.box.lines}` : '—', 'text box (chars × lines)')));
      if (st.speakers && st.speakers.length) {
        card.append(h('div', { class: 'speakers' }, st.speakers.slice(0, 24).map(([n, c]) => h('span', null, n, h('em', { text: num(c) })))));
      }
      card.append(h('div', { class: 'row-actions' }, h('button', { class: 'btn', onclick: () => show('translator') }, 'Next: choose the translator')));
    }
    box.replaceChildren(card);
  }
  function isTyping(root) {
    const a = document.activeElement;
    return a && root.contains(a) && /^(INPUT|TEXTAREA|SELECT)$/.test(a.tagName);
  }
  function stat(v, l) { return h('div', { class: 'stat' }, h('b', { text: v }), h('span', { text: l })); }

  function titlePicker() {
    const wrap = h('div', { class: 'title-pick' },
      h('b', { text: 'This game\'s archives are encrypted. Which game is it?' }),
      h('p', { class: 'hint', text: 'Yakusu knows the encryption of about 500 Kirikiri games. Search by English or Japanese title.' }));
    const input = h('input', { class: 'input', type: 'search', placeholder: 'Search titles', 'aria-label': 'Search game titles' });
    const list = h('ul');
    let t;
    input.addEventListener('input', () => { clearTimeout(t); t = setTimeout(search, 200); });
    async function search() {
      const r = await api('/api/xp3_games?q=' + encodeURIComponent(input.value.trim()));
      list.replaceChildren(...r.games.map((g) => h('li', null, h('button', { onclick: () => safe(async () => {
        await api('/api/engine', { xp3_title: g.name }); toast(`Using the profile for ${g.name}.`); run('extract');
      }) }, g.name, g.title ? h('span', { class: 'hint', text: '  ' + g.title }) : null))));
    }
    wrap.append(input, list);
    search();
    return wrap;
  }

  // ------------------------------------------------------------ 2. translator
  function renderTranslator() {
    if (view !== 'translator') return;
    if (isTyping($('section[data-view="translator"]'))) return;
    const s = S.settings;
    const seg = $('#providerSeg');
    seg.replaceChildren(...Object.entries(S.providers).map(([id, p]) => h('button', {
      'aria-pressed': String(s.provider === id), role: 'radio', 'aria-checked': String(s.provider === id),
      onclick: () => safe(async () => { await api('/api/settings', { provider: id }); estimates = null; await refresh(); loadEstimates(); }),
    }, p.label)));
    const prov = S.providers[s.provider];
    const f = $('#providerFields');
    f.replaceChildren();
    if (s.provider !== 'local') {
      const key = h('input', { class: 'input', type: 'password', id: 'apiKey', autocomplete: 'off', spellcheck: 'false',
        placeholder: s.keys[s.provider] ? `Saved (${s.keys[s.provider]}). Paste a new key to replace it.` : `Paste your key (${prov.key_hint})` });
      const status = h('div', { class: 'key-status' });
      f.append(h('div', { class: 'field-group' }, h('label', { class: 'label', for: 'apiKey', text: 'API key' }),
        h('div', { class: 'key-row' }, key,
          h('button', { class: 'btn', onclick: () => safe(async () => {
            if (key.value.trim()) await api('/api/settings', { provider: s.provider, api_key: key.value.trim() });
            key.value = '';
            status.className = 'key-status'; status.textContent = 'Checking…';
            try { const r = await api('/api/test_key', {}); status.className = 'key-status ok'; status.textContent = `Key works (${r.model}).`; }
            catch (e) { status.className = 'key-status bad'; status.textContent = e.message; }
            refresh();
          }) }, 'Save and test')),
        prov.key_url ? h('p', { class: 'hint' }, 'Get a key at ', h('a', { href: prov.key_url, target: '_blank', rel: 'noopener', text: prov.key_url.replace('https://', '') }), '. It\'s stored only on this computer.') : null,
        status));
    }
    if (s.provider !== 'anthropic') {
      const tm = h('input', { class: 'input', id: 'tmodel', value: (s.translate_model || {})[s.provider] || '', placeholder: s.provider === 'local' ? 'Model name as shown in LM Studio' : 'Model ID, e.g. from your provider\'s docs' });
      const rm = h('input', { class: 'input', id: 'rmodel', value: (s.review_model || {})[s.provider] || '', placeholder: 'Optional, a stronger model for the review pass' });
      const bu = h('input', { class: 'input', id: 'burl', value: (s.base_urls || {})[s.provider] || prov.base_url });
      const save = () => safe(async () => { await api('/api/settings', { provider: s.provider, translate_model: tm.value, review_model: rm.value, base_url: bu.value, review: !!rm.value.trim() }); toast('Saved.'); refresh(); });
      [tm, rm, bu].forEach((x) => x.addEventListener('change', save));
      f.append(h('div', { class: 'two' },
        h('div', { class: 'field-group' }, h('label', { class: 'label', for: 'tmodel', text: 'Translation model' }), tm),
        h('div', { class: 'field-group' }, h('label', { class: 'label', for: 'rmodel', text: 'Review model' }), rm)),
        h('div', { class: 'field-group' }, h('label', { class: 'label', for: 'burl', text: 'API address' }), bu,
          h('p', { class: 'hint', text: s.provider === 'local' ? 'Start the local server in LM Studio (Developer tab) first. Local models are free but slower, and quality depends on the model.' : 'Cost estimates are only shown for Claude models. Yakusu still counts the tokens it uses.' })));
    }
    $('#presetGroup').hidden = s.provider !== 'anthropic';
    renderPresets();
    renderAdvanced();
    const b = $('#budget');
    if (document.activeElement !== b) b.value = s.budget_usd || '';
  }
  $('#budget').addEventListener('change', (e) => safe(() => api('/api/settings', { budget_usd: parseFloat(e.target.value) || 0 })));
  const ADV = [['#batchSize', 'batch_size', '#bsVal', 40], ['#parallel', 'parallel', '#parVal', 4], ['#contextLines', 'context_lines', '#ctxVal', 10]];
  function renderAdvanced() {
    for (const [id, key, lab, def] of ADV) {
      if (document.activeElement !== $(id)) $(id).value = S.settings[key] != null ? S.settings[key] : def;
      $(lab).textContent = $(id).value;
    }
  }
  for (const [id, key, lab] of ADV) {
    $(id).addEventListener('input', () => { $(lab).textContent = $(id).value; });
    $(id).addEventListener('change', () => safe(async () => { await api('/api/settings', { [key]: +$(id).value }); estimates = null; loadEstimates(); }));
  }

  function renderPresets() {
    const s = S.settings;
    const box = $('#presets');
    box.replaceChildren(...Object.entries(S.presets).map(([id, p]) => {
      const est = estimates && estimates[id];
      return h('button', { class: 'preset', role: 'radio', 'aria-checked': String(s.preset === id),
        onclick: () => safe(async () => { await api('/api/settings', { preset: id, review: !!p.review }); refresh(); }) },
        h('b', { text: p.label }),
        h('span', { class: 'models', text: p.review ? `${p.translate} + ${p.review}` : p.translate }),
        h('span', { class: 'blurb', text: p.blurb }),
        h('span', { class: 'cost', text: est && est.total ? cap(range(est.total)) : (S.project && S.project.stats ? '…' : 'Read the scripts to see the cost') }));
    }));
  }

  async function loadEstimates() {
    if (!S || !S.project || !S.project.stats) return;
    try { estimates = await api('/api/estimate?all=1'); } catch (e) { estimates = null; }
    renderPresets();
    renderRun();
    if (view === 'run') loadNotes();
  }
  async function loadNotes() {
    let r;
    try { r = await api('/api/preflight'); } catch (e) { return; }
    const items = (r.notes || []).map((n) => h('li', { class: n.level }, n.text));
    if (r.glossary_issues) {
      items.push(h('li', { class: 'warn' }, `${num(r.glossary_issues)} line${r.glossary_issues === 1 ? ' doesn\'t' : 's don\'t'} use the glossary's spelling of a name or term.`,
        h('button', { class: 'btn btn-quiet', onclick: () => { linesState.filter = 'problems'; show('lines'); } }, 'See them'),
        h('button', { class: 'btn', onclick: () => run('glossary') }, 'Fix them')));
    }
    if (r.overflow && S.project.translated) {
      items.push(h('li', { class: 'warn' }, `${num(r.overflow)} line${r.overflow === 1 ? '' : 's'} won't fit the text box.`,
        h('button', { class: 'btn', onclick: () => run('fit') }, 'Shorten them')));
    }
    $('#runNotes').replaceChildren(...items);
  }

  // ------------------------------------------------------------ 3. style
  async function loadStyle() {
    if (!S || !S.project) return;
    const r = await api('/api/style');
    $('#styleText').value = r.style || '';
    const names = Object.entries((r.glossary && r.glossary.names) || {});
    const speakers = ((S.project.stats || {}).speakers || []).map(([n]) => n);
    for (const sp of speakers) if (!names.find(([j]) => j === sp)) names.push([sp, '']);
    $('#namesBody').replaceChildren(...names.map(([j, e]) => pairRow(j, e)));
    $('#termsBody').replaceChildren(...(((r.glossary && r.glossary.terms) || []).map((t) => pairRow(t.jp, t.en))));
  }
  function pairRow(j, e) {
    return h('tr', null, h('td', null, h('input', { value: j || '', 'aria-label': 'Japanese', lang: 'ja' })), h('td', null, h('input', { value: e || '', 'aria-label': 'English' })));
  }
  $('#addName').addEventListener('click', () => $('#namesBody').append(pairRow('', '')));
  $('#addTerm').addEventListener('click', () => $('#termsBody').append(pairRow('', '')));
  $('#draftStyle').addEventListener('click', () => {
    if (S.project && S.project.has_style && !confirmInline('style', 'Press again to replace your style sheet and add any new names.')) return;
    run('style');
  });
  let styleTimer;
  function saveStyleSoon() {
    $('#styleSaved').textContent = 'Saving…';
    clearTimeout(styleTimer);
    styleTimer = setTimeout(() => safe(async () => {
      const pairs = (tb) => $$('tr', tb).map((tr) => $$('input', tr).map((i) => i.value.trim())).filter(([j]) => j);
      const names = {};
      for (const [j, e] of pairs($('#namesBody'))) names[j] = e;
      const terms = pairs($('#termsBody')).map(([jp, en]) => ({ jp, en }));
      await api('/api/style', { style: $('#styleText').value, glossary: { names, terms } });
      $('#styleSaved').textContent = 'Saved. Every batch from now on uses these notes.';
    }), 700);
  }
  $('section[data-view="style"]').addEventListener('input', (e) => {
    if (e.target.matches('#styleText, .names input')) saveStyleSoon();
  });

  // ------------------------------------------------------------ 4. test
  function renderTest() {
    if (view !== 'test' || !S.project) return;
    const p = S.project;
    const sel = $('#testFile');
    if (document.activeElement !== sel) {
      const cur = p.test_file || '';
      sel.replaceChildren(h('option', { value: '', text: 'First scene (automatic)' }),
        ...(p.files || []).map((f) => h('option', { value: f, text: f.replace(/^ks\//, '').replace(/\.json$/, ''), selected: f === cur })));
    }
    const box = p.box || { width: 56, lines: 3 };
    for (const [id, val, lab] of [['#boxWidth', box.width, '#widthVal'], ['#boxLines', box.lines, '#linesVal']]) {
      if (document.activeElement !== $(id)) $(id).value = val;
      $(lab).textContent = $(id).value;
    }
    const st = p.settings || {};
    if (document.activeElement !== $('#quotes')) $('#quotes').value = st.quotes || 'auto';
    $('#insertBreaks').checked = st.insert_breaks !== false;
    $('#boxHint').textContent = box.user_set ? 'Using your text box size.' :
      (box.mode === 'explicit' ? `Measured from the original script: lines are up to ${box.width} half-width characters, ${box.lines} lines per box.` :
        'This game wraps text itself. The width is an estimate; adjust it if words get split in the game.');
  }
  $('#testFile').addEventListener('change', (e) => safe(() => api('/api/box', { test_file: e.target.value || null })));
  $('#runTest').addEventListener('click', () => run('test'));
  let boxTimer;
  function boxChanged() {
    $('#widthVal').textContent = $('#boxWidth').value;
    $('#linesVal').textContent = $('#boxLines').value;
    clearTimeout(boxTimer);
    boxTimer = setTimeout(() => safe(async () => {
      await api('/api/box', { width: +$('#boxWidth').value, lines: +$('#boxLines').value, quotes: $('#quotes').value, insert_breaks: $('#insertBreaks').checked });
      await loadPreview();
    }), 250);
  }
  ['#boxWidth', '#boxLines'].forEach((id) => $(id).addEventListener('input', boxChanged));
  ['#quotes', '#insertBreaks'].forEach((id) => $(id).addEventListener('change', boxChanged));
  $('#applyFit').addEventListener('click', () => run('refit'));
  $('#launch1').addEventListener('click', launch);
  $('#restore1').addEventListener('click', () => run('restore'));
  $('#prevLine').addEventListener('click', () => { previewIdx = Math.max(0, previewIdx - 1); drawPreview(); });
  $('#nextLine').addEventListener('click', () => { previewIdx = Math.min(previewLines.length - 1, previewIdx + 1); drawPreview(); });

  async function loadPreview() {
    if (!S || !S.project) return;
    try { const r = await api('/api/preview'); previewLines = r.lines || []; previewBox = r.box; } catch (e) { previewLines = []; }
    previewIdx = Math.min(previewIdx, Math.max(0, previewLines.length - 1));
    drawPreview();
  }
  function drawPreview() {
    const tb = $('#tbText');
    const box = previewBox || (S.project && S.project.box) || { width: 56, lines: 3 };
    // Size the preview font so 'width' half-width characters fill the box.
    const inner = $('#textbox').clientWidth * 0.93;
    tb.style.fontSize = Math.max(9, Math.min(44, (inner / (box.width * 0.55)))) + 'px';
    tb.style.minHeight = `calc(${box.lines} * 1.6em)`;
    const ln = previewLines[previewIdx];
    if (!ln) {
      $('#nameplate').textContent = '';
      tb.replaceChildren(h('span', { class: 'row', text: 'Translated lines appear here, wrapped exactly as they will be in the game.' }));
      $('#tbOver').textContent = '';
      $('#lineCounter').textContent = '';
      return;
    }
    $('#nameplate').textContent = ln.name || '';
    tb.replaceChildren(...ln.lines.map((r, i) => h('span', { class: 'row' + (i >= box.lines ? ' overflow' : ''), text: r || ' ' })));
    $('#tbOver').textContent = ln.over ? `${ln.over} line${ln.over > 1 ? 's' : ''} too long` : '';
    $('#lineCounter').textContent = `${previewIdx + 1} / ${previewLines.length}`;
  }
  window.addEventListener('resize', drawPreview);

  // ------------------------------------------------------------ 5. run
  function renderRun() {
    if (view !== 'run' || !S.project) return;
    const p = S.project;
    const sum = $('#runSummary');
    const pct = (a, b) => (b ? Math.round((100 * a) / b) : 0);
    const row = (label, a, b, cls) => h('div', { class: 'progress-row' }, h('span', { text: label }),
      h('div', { class: 'bar ' + (cls || '') }, h('i', { style: `width:${pct(a, b)}%` })), h('span', { class: 'num', text: `${num(a)} / ${num(b)}` }));
    const kids = [row('Translated', p.translated, p.total)];
    if (S.settings.review !== false) kids.push(row('Reviewed', p.reviewed, p.total, 'ok'));
    const est = estimates && estimates[S.settings.preset];
    const remaining = p.total - p.translated;
    kids.push(h('div', { class: 'estimate' },
      remaining > 0 ? [h('b', { text: num(remaining) }), ' lines left. ',
        est && est.total ? ['Estimated cost for the rest: ', h('b', { text: range(est.total) }), '. '] : null] : 'Every line is translated. ',
      'Spent so far: ', h('b', { text: money(p.spent) }), '.',
      p.problems ? [' ', h('a', { href: '#', onclick: (e) => { e.preventDefault(); linesState.filter = 'problems'; show('lines'); } }, `${p.problems} lines need a look`), '.'] : null));
    if (p.installed) kids.push(h('div', { class: 'installed', text: 'The patch is installed. Launch the game to play in English.' }));
    sum.replaceChildren(...kids);
    $('#runFull').textContent = p.translated ? 'Continue translating' : 'Translate everything';
    if (p.translated >= p.total && p.total) $('#runFull').textContent = 'Rebuild and install';
    const mn = $('#manualNote');
    mn.replaceChildren();
    if (p.manual_output) {
      mn.append(h('div', { class: 'callout' }, 'This engine needs one manual step. The patched script files are in ',
        h('code', { text: p.manual_output }), '. ', (p.engine && p.engine.note) || '', ' ',
        h('button', { class: 'btn btn-quiet', onclick: () => safe(() => api('/api/reveal', { which: 'manual' })) }, 'Open folder')));
    }
  }
  $('#runFull').addEventListener('click', () => {
    const p = S.project;
    run(p && p.translated >= p.total && p.total ? 'build' : 'full');
  });
  $('#launch2').addEventListener('click', launch);
  $$('[data-action]').forEach((b) => b.addEventListener('click', () => run(b.dataset.action)));

  function launch() { safe(async () => { const r = await api('/api/launch', {}); toast(`Started ${r.launched}.`); }); }
  function run(action) {
    safe(async () => {
      if (!S.project) throw new Error('Choose a game folder first.');
      if (action === 'restore' && !confirmInline('restore', 'Press again to restore the original game files.')) return;
      await api('/api/run', { action });
      await refresh();
    });
  }
  let armed = null;
  function confirmInline(what, message) {
    // Ask by making the user press twice, instead of a blocking dialog.
    if (armed === what) { armed = null; return true; }
    armed = what;
    toast(message);
    setTimeout(() => { if (armed === what) armed = null; }, 4000);
    return false;
  }

  // ------------------------------------------------------------ lines
  $$('#lineFilter button').forEach((b) => b.addEventListener('click', () => {
    linesState.filter = b.dataset.f; linesState.page = 0; loadLines();
  }));
  let qTimer;
  $('#lineSearch').addEventListener('input', (e) => { clearTimeout(qTimer); qTimer = setTimeout(() => { linesState.q = e.target.value; linesState.page = 0; loadLines(); }, 250); });

  let lastRows = [];
  $('#retranslatePage').addEventListener('click', () => safe(async () => {
    const ids = lastRows.filter((r) => !r.skip).map((r) => r.id);
    await api('/api/retranslate', { ids });
    refresh();
  }));
  $('#exportCsv').addEventListener('click', () => safe(async () => {
    const r = await api('/api/export_csv', {});
    toast(`Saved ${r.path.split(/[\\/]/).pop()} in the project's exports folder.`);
  }));
  $('#importCsv').addEventListener('change', (e) => safe(async () => {
    const f = e.target.files[0];
    e.target.value = '';
    if (!f) return;
    const r = await api('/api/import_csv', { text: await f.text() });
    toast(`Imported ${num(r.changed)} edited line${r.changed === 1 ? '' : 's'}` + (r.problems ? `; ${r.problems} need a look` : '') +
      (r.unknown ? `; ${r.unknown} rows didn't match this game` : '') + '. Rebuild to put them in the game.');
    loadLines();
  }));

  async function loadLines() {
    $$('#lineFilter button').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.f === linesState.filter)));
    $('#retranslatePage').hidden = !['problems', 'untranslated', 'reviewed'].includes(linesState.filter);
    if (!S || !S.project || !S.project.extracted) {
      $('#linesList').replaceChildren(h('div', { class: 'empty-state', text: 'Open a game and read its scripts to see the lines here.' }));
      $('#pager').replaceChildren();
      return;
    }
    const r = await api(`/api/lines?filter=${linesState.filter}&q=${encodeURIComponent(linesState.q)}&page=${linesState.page}`);
    const list = $('#linesList');
    lastRows = r.rows;
    const nr = r.rows.filter((x) => !x.skip).length;
    $('#retranslatePage').textContent = nr === 1 ? 'Retranslate this line' : `Retranslate these ${num(nr)} lines`;
    $('#retranslatePage').disabled = !nr;
    if (!r.rows.length) {
      list.replaceChildren(h('div', { class: 'empty-state', text: 'No lines match.' }));
    } else {
      list.replaceChildren(...r.rows.map(lineRow));
    }
    const pages = Math.ceil(r.total / r.per);
    $('#pager').replaceChildren(h('span', { text: `${num(r.total)} line${r.total === 1 ? '' : 's'}` }),
      h('button', { class: 'btn btn-quiet', disabled: r.page <= 0, onclick: () => { linesState.page--; loadLines(); } }, 'Previous'),
      h('span', { text: `Page ${r.page + 1} of ${Math.max(1, pages)}` }),
      h('button', { class: 'btn btn-quiet', disabled: r.page + 1 >= pages, onclick: () => { linesState.page++; loadLines(); } }, 'Next'));
  }
  function lineRow(r) {
    const ta = h('textarea', { 'aria-label': 'English', placeholder: r.skip ? '' : 'Not translated yet' });
    ta.value = r.en || '';
    const meta = h('div', { class: 'meta' });
    const stageName = { translate: 'translated', review: 'improved in review', reviewed: 'reviewed', fit: 'shortened to fit', manual: 'edited by you' }[r.stage];
    if (stageName) meta.append(stageName);
    if (r.problems && r.problems.length) meta.append(' · ', h('span', { class: 'bad', text: r.problems.join(', ') }));
    if (r.glossary && r.glossary.length) meta.append(' · ', h('span', { class: 'gloss', text: `should use: ${r.glossary.join(', ')}` }));
    if (r.skip) meta.append(`Skipped: ${r.skip}`);
    const before = r.before && r.before !== r.en ? h('div', { class: 'before', title: 'Before review', text: r.before }) : null;
    ta.addEventListener('change', () => safe(async () => {
      const res = await api('/api/line', { id: r.id, en: ta.value });
      meta.replaceChildren(res.problems && res.problems.length
        ? h('span', { class: 'bad', text: `Saved, but check it: ${res.problems.join(', ')}` })
        : 'Saved. Rebuild the patch to put it in the game.');
    }));
    return h('div', { class: 'ln' + (r.skip ? ' skipped' : '') },
      h('div', { class: 'who', title: r.file, text: r.who || 'narration' }),
      h('div', { class: 'src', lang: 'ja', text: r.jp }),
      h('div', null, ta, before, meta),
      h('div', { class: 'actions' },
        r.skip ? null : h('button', { class: 'skipbtn', title: 'Translate this line again', 'aria-label': 'Retranslate',
          onclick: () => safe(async () => { await api('/api/retranslate', { ids: [r.id] }); refresh(); }) }, '↻'),
        h('button', { class: 'skipbtn', title: r.skip ? 'Translate this line' : 'Don\'t translate this line',
          onclick: () => safe(async () => { await api('/api/line', { id: r.id, skip: !r.skip }); loadLines(); }) }, r.skip ? '↺' : '×')));
  }

  // ------------------------------------------------------------ log
  function renderLog() {
    const el = $('#log');
    const atBottom = el.scrollTop + el.clientHeight >= el.scrollHeight - 20;
    if (!(S && S.logs.length)) { el.textContent = 'Nothing yet.'; return; }
    el.replaceChildren(...S.logs.map((l) => h('div', { class: /^(Error|Traceback)|failed|Couldn/.test(l.m) ? 'err' : '' },
      new Date(l.t * 1000).toLocaleTimeString() + '  ' + l.m)));
    if (atBottom) el.scrollTop = el.scrollHeight;
  }

  // ------------------------------------------------------------ start
  refresh().then(() => {
    const p = S.project;
    if (p && p.extracted) show(p.translated ? 'run' : (S.settings.keys[S.settings.provider] ? 'style' : 'translator'));
    else show('game');
    loop();
  });
})();
