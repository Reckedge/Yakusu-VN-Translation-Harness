/* Picks the theme before the page paints. "light" is the default.
   ?theme=<name> previews any theme without saving it. */
(function () {
  var LIGHT = 'haru';
  var saved = 'light';
  try { saved = localStorage.getItem('yakusu-theme') || 'light'; } catch (e) { /* private mode */ }
  var preview = new URLSearchParams(location.search).get('theme');
  var pick = preview || saved;
  document.documentElement.setAttribute('data-theme', pick === 'light' ? LIGHT : pick);
  window.__yakusuTheme = {
    light: LIGHT,
    current: function () { return saved; },
    set: function (t) {
      saved = t;
      try { localStorage.setItem('yakusu-theme', t); } catch (e) { /* ignore */ }
      document.documentElement.setAttribute('data-theme', t === 'light' ? LIGHT : t);
    },
  };
})();
