/* Syntra dashboard runtime.
 *
 * Polls /api/state every 2 seconds and reconciles the DOM. Renders the
 * header pill + meta line, drives the reward chart in chart.js, paints
 * the distribution + recent-decisions feed, and now (Phase 2) the
 * live-kernel-outputs panel and the capsule switcher.
 *
 * Design tenets:
 *  - never destroy and rebuild rows when an update would do (avoids
 *    flicker, keeps the fade-in animation meaningful)
 *  - never block on a failed poll — show the last good state, surface
 *    the error in the page meta line
 *  - no external libs
 */
(function () {
  'use strict';

  var POLL_INTERVAL_MS = 2000;

  // Default candidate set, mirrors Lang/src/meta_bandit.rs CandidateId::all().
  // Used so the chart can show empty placeholders even before the first
  // /api/state arrives.
  var META_BANDIT_NAMES = [
    'Thompson', 'Ucb', 'EpsilonGreedy', 'Weighted', 'Greedy', 'LinUcb', 'LinTs'
  ];

  var $ = function (id) { return document.getElementById(id); };

  // ----------------------------------------------------------------- //
  // Module state                                                      //
  // ----------------------------------------------------------------- //
  //
  // `capsuleIndex` maps "tenant/job/capsule" -> { name, options, scoringMode }.
  // Populated by /api/capsules, refreshed in the background; the poller
  // reads from it on every tick.
  var capsuleIndex = {};
  // Currently selected capsule path. Drives the /api/state query.
  var selectedCapsule = null;
  // Sparkline svg cache keyed by published-name so we don't re-create
  // SVG nodes every poll.
  var sparkSvgs = {};

  // ----------------------------------------------------------------- //
  // Capsule path / hash plumbing                                      //
  // ----------------------------------------------------------------- //

  function capsuleFromHash() {
    var h = window.location.hash || '';
    var m = h.match(/(?:^#|&)capsule=([^&]+)/);
    if (!m) return null;
    try {
      return decodeURIComponent(m[1]);
    } catch (e) {
      return m[1];
    }
  }

  function setCapsuleHash(path) {
    if (!path) {
      // Avoid leaving "#" behind — clear hash silently.
      if (window.location.hash) {
        history.replaceState(null, '', window.location.pathname + window.location.search);
      }
      return;
    }
    var next = '#capsule=' + encodeURIComponent(path);
    if (window.location.hash !== next) {
      // Use replaceState so back-button doesn't get noisy.
      history.replaceState(null, '', window.location.pathname + window.location.search + next);
    }
  }

  function optionsForCapsule(path) {
    var entry = capsuleIndex[path];
    if (entry && Array.isArray(entry.options)) return entry.options;
    return null;
  }

  function labelForOption(path, idx) {
    if (typeof idx !== 'number') return null;
    var opts = optionsForCapsule(path);
    if (opts && idx >= 0 && idx < opts.length) return opts[idx];
    return 'option#' + idx;
  }

  // ----------------------------------------------------------------- //
  // Capsule switcher                                                  //
  // ----------------------------------------------------------------- //

  function populateSwitcher(list) {
    var sel = $('capsule-select');
    if (!sel) return;
    // Preserve current selection if still in the list.
    var current = selectedCapsule;
    // Rebuild options — list is small (~3 entries) so this is fine.
    var frag = document.createDocumentFragment();
    if (!list.length) {
      var blank = document.createElement('option');
      blank.value = '';
      blank.textContent = '(no capsules installed)';
      frag.appendChild(blank);
    } else {
      for (var i = 0; i < list.length; i++) {
        var c = list[i];
        var opt = document.createElement('option');
        opt.value = c.path;
        opt.textContent = c.name ? (c.name + '  ·  ' + c.path) : c.path;
        if (c.path === current) opt.selected = true;
        frag.appendChild(opt);
      }
    }
    sel.innerHTML = '';
    sel.appendChild(frag);
    if (current) sel.value = current;
  }

  function loadCapsules() {
    return fetch('/api/capsules', { cache: 'no-store' })
      .then(function (r) { return r.json(); })
      .then(function (data) {
        var list = (data && data.capsules) || [];
        capsuleIndex = {};
        for (var i = 0; i < list.length; i++) {
          var c = list[i];
          if (c && c.path) capsuleIndex[c.path] = c;
        }
        populateSwitcher(list);
        return list;
      })
      .catch(function () {
        capsuleIndex = {};
        populateSwitcher([]);
        return [];
      });
  }

  // ----------------------------------------------------------------- //
  // Header                                                            //
  // ----------------------------------------------------------------- //

  function setHeader(state) {
    $('capsulePath').textContent = state.capsulePath || '—';

    var pill = $('lifecyclePill');
    var pillState = $('lifecycleState');
    var pillDetail = $('lifecycleDetail');

    var lifecycle = (state.lifecycle || 'unknown').toLowerCase();
    pill.setAttribute('data-state', lifecycle);

    if (lifecycle === 'warmup') {
      pillState.textContent = 'WARMUP';
      var wp = state.warmupProgress;
      pillDetail.textContent = wp
        ? (wp.collected + ' / ' + wp.target)
        : '';
    } else if (lifecycle === 'active') {
      pillState.textContent = 'ACTIVE';
      pillDetail.textContent = state.algorithm
        ? ('algorithm: ' + state.algorithm)
        : '';
    } else if (lifecycle === 'frozen') {
      pillState.textContent = 'FROZEN';
      pillDetail.textContent = state.algorithm || '';
    } else {
      pillState.textContent = lifecycle.toUpperCase();
      pillDetail.textContent = '';
    }

    // Header meta line.
    var lastUpdateAgo = '—';
    if (typeof state.lastUpdateAt === 'number' && state.serverNow) {
      var ago = Math.max(0, Math.round((state.serverNow - state.lastUpdateAt) / 1000));
      lastUpdateAgo = ago + 's ago';
    }
    var meta = 'decisions: ' + (state.totalDecisions || 0)
      + ' · refused: ' + (state.refusedCount || 0)
      + ' · last update: ' + lastUpdateAgo;
    if (state.errors && state.errors.length) {
      meta += ' · ' + state.errors[0];
    }
    $('headerMeta').textContent = meta;
  }

  // ----------------------------------------------------------------- //
  // Chart subtitle reflects scoring mode                              //
  // ----------------------------------------------------------------- //

  function setChartChrome(state) {
    var sub = $('chartSubtitle');
    if (state.scoringMode === 'shared-state-linucb') {
      sub.textContent = 'last 5 minutes · shared-state LinUCB';
    } else if (state.scoringMode === 'hierarchical') {
      sub.textContent = 'last 5 minutes · per-HierState meta-bandits';
    } else {
      sub.textContent = 'last 5 minutes · per candidate algorithm';
    }
  }

  // ----------------------------------------------------------------- //
  // Distribution bars (Region 3)                                       //
  // ----------------------------------------------------------------- //

  function renderDistribution(state) {
    var root = $('distribution');
    var empty = $('distEmpty');
    var rows = state.decisionCounts || [];
    var path = state.capsulePath;

    if (rows.length === 0) {
      if (empty) empty.style.display = 'block';
      var stale = root.querySelectorAll('.dist-row');
      for (var i = 0; i < stale.length; i++) root.removeChild(stale[i]);
      return;
    }
    if (empty) empty.style.display = 'none';

    var maxCount = 0;
    for (var k = 0; k < rows.length; k++) {
      if (rows[k].count > maxCount) maxCount = rows[k].count;
    }
    if (maxCount < 1) maxCount = 1;

    var leaderIdx = rows[0].optionIndex;
    var existing = {};
    var nodes = root.querySelectorAll('.dist-row');
    for (var n = 0; n < nodes.length; n++) {
      existing[nodes[n].getAttribute('data-option')] = nodes[n];
    }

    var lastNode = empty;
    for (var r = 0; r < rows.length; r++) {
      var row = rows[r];
      var key = 'opt_' + row.optionIndex;
      var node = existing[key];
      if (!node) {
        node = document.createElement('div');
        node.className = 'dist-row';
        node.setAttribute('data-option', key);
        node.innerHTML = '<span class="opt-name"></span>'
          + '<div class="bar-track"><div class="bar-fill"></div></div>'
          + '<span class="opt-count"></span>';
        root.appendChild(node);
      } else {
        delete existing[key];
      }
      if (lastNode && lastNode.nextSibling !== node) {
        root.insertBefore(node, lastNode.nextSibling);
      }
      lastNode = node;

      node.querySelector('.opt-name').textContent = labelForOption(path, row.optionIndex);
      node.querySelector('.opt-count').textContent = String(row.count);
      var pct = (row.count / maxCount) * 100;
      node.querySelector('.bar-fill').style.width = pct.toFixed(1) + '%';
      node.classList.toggle('is-leader', row.optionIndex === leaderIdx);
    }

    for (var stale2 in existing) {
      root.removeChild(existing[stale2]);
    }
  }

  // ----------------------------------------------------------------- //
  // Recent decisions feed (Region 4)                                  //
  // ----------------------------------------------------------------- //

  function formatTime(observedAt, now) {
    if (typeof observedAt !== 'number') return '—';
    var ageMs = Math.max(0, now - observedAt);
    var ageSec = Math.round(ageMs / 1000);
    if (ageSec < 60) return ageSec + 's ago';
    var d = new Date(observedAt);
    var hh = String(d.getHours()).padStart(2, '0');
    var mm = String(d.getMinutes()).padStart(2, '0');
    var ss = String(d.getSeconds()).padStart(2, '0');
    return hh + ':' + mm + ':' + ss;
  }

  function colourForOption(rank) {
    return rank === 0 ? '#22d3ee' : '#374151';
  }

  function renderFeed(state) {
    var root = $('feed');
    var entries = state.recentDecisions || [];
    var path = state.capsulePath;

    if (entries.length === 0) {
      if (!root.querySelector('.empty')) {
        root.innerHTML = '<li class="empty">Waiting for the first decision.</li>';
      }
      return;
    }

    var counts = state.decisionCounts || [];
    var optionRank = {};
    for (var ci = 0; ci < counts.length; ci++) {
      optionRank[counts[ci].optionIndex] = ci;
    }

    var emptyLi = root.querySelector('.empty');
    if (emptyLi) root.removeChild(emptyLi);

    var fragment = document.createDocumentFragment();
    var now = state.serverNow || Date.now();

    for (var i = 0; i < entries.length; i++) {
      var e = entries[i];
      var id = e.id || ('idx_' + i);
      var node = document.createElement('li');
      node.className = 'feed-row';
      node.setAttribute('data-id', id);
      node.innerHTML = '<span class="feed-time"></span>'
        + '<span class="dot"></span>'
        + '<span class="feed-body">'
        + '<span class="feed-option"></span>'
        + '<span class="feed-reason"></span>'
        + '</span>';

      node.querySelector('.feed-time').textContent = formatTime(e.observedAt, now);
      var optEl = node.querySelector('.feed-option');
      var reasonEl = node.querySelector('.feed-reason');
      var dot = node.querySelector('.dot');

      if (e.refused) {
        node.classList.add('is-refused');
        optEl.textContent = 'REFUSED';
        reasonEl.textContent = e.refusalReason || 'reason unknown';
      } else {
        node.classList.remove('is-refused');
        optEl.textContent = labelForOption(path, e.optionIndex);
        reasonEl.textContent = e.algorithm ? ('via ' + e.algorithm) : '';
        var rank = (typeof e.optionIndex === 'number' && optionRank[e.optionIndex] != null)
          ? optionRank[e.optionIndex] : 99;
        dot.style.background = colourForOption(rank);
      }
      fragment.appendChild(node);
    }

    root.innerHTML = '';
    root.appendChild(fragment);
  }

  // ----------------------------------------------------------------- //
  // Live kernel outputs (Region 5)                                    //
  // ----------------------------------------------------------------- //

  function formatPubValue(v) {
    if (v == null) return '—';
    if (typeof v === 'number') {
      if (!isFinite(v)) return String(v);
      if (Math.abs(v) >= 1000) return v.toFixed(0);
      if (Math.abs(v) >= 1) return v.toFixed(2);
      return v.toFixed(4);
    }
    if (typeof v === 'boolean') return v ? 'true' : 'false';
    return String(v);
  }

  function isNumericSeries(arr) {
    if (!arr || !arr.length) return false;
    for (var i = 0; i < arr.length; i++) {
      if (typeof arr[i] === 'number' && isFinite(arr[i])) return true;
    }
    return false;
  }

  function renderPublished(state) {
    var grid = $('publishedGrid');
    var empty = $('publishedEmpty');
    var latest = state.publishedLatest || {};
    var series = state.publishedSeries || {};

    var keys = Object.keys(latest);
    // Keep stable visual order — alphabetical.
    keys.sort();

    if (keys.length === 0) {
      // Wipe any leftover cells, show placeholder.
      var cells = grid.querySelectorAll('.published-cell');
      for (var i = 0; i < cells.length; i++) grid.removeChild(cells[i]);
      sparkSvgs = {};
      if (empty) empty.style.display = '';
      return;
    }
    if (empty) empty.style.display = 'none';

    var existing = {};
    var nodes = grid.querySelectorAll('.published-cell');
    for (var n = 0; n < nodes.length; n++) {
      existing[nodes[n].getAttribute('data-name')] = nodes[n];
    }

    var lastNode = empty;
    for (var k = 0; k < keys.length; k++) {
      var name = keys[k];
      var value = latest[name];
      var node = existing[name];
      if (!node) {
        node = document.createElement('div');
        node.className = 'published-cell';
        node.setAttribute('data-name', name);
        node.innerHTML = '<span class="pub-name"></span>'
          + '<span class="pub-value"></span>'
          + '<svg class="pub-spark" viewBox="0 0 100 24" preserveAspectRatio="none" aria-hidden="true"></svg>';
        grid.appendChild(node);
      } else {
        delete existing[name];
      }
      if (lastNode && lastNode.nextSibling !== node) {
        grid.insertBefore(node, lastNode.nextSibling);
      }
      lastNode = node;

      node.querySelector('.pub-name').textContent = name;
      node.querySelector('.pub-value').textContent = formatPubValue(value);
      // Strings/booleans get a different visual treatment.
      node.classList.toggle('is-text', typeof value !== 'number');

      var svg = node.querySelector('.pub-spark');
      sparkSvgs[name] = svg;
      var arr = series[name];
      if (isNumericSeries(arr)) {
        svg.style.display = '';
        window.SyntraChart.renderSparkline(svg, arr);
      } else {
        // Non-numeric publishes (e.g. status strings) have no sparkline.
        svg.style.display = 'none';
      }
    }

    // Cells for keys that disappeared (rare — typically when switching capsule).
    for (var stale in existing) {
      grid.removeChild(existing[stale]);
      delete sparkSvgs[stale];
    }
  }

  // ----------------------------------------------------------------- //
  // Chart push                                                        //
  // ----------------------------------------------------------------- //

  function pushChartSamples(chart, state) {
    var samples = [];

    if (state.scoringMode === 'shared-state-linucb' && state.sharedState) {
      samples.push({
        name: 'SharedStateLinUcb',
        mean: state.sharedState.meanReward || 0,
        trials: state.sharedState.trials || 0
      });
    } else if (state.scoringMode === 'hierarchical' && state.hierarchical) {
      // Hierarchical capsules carry one meta-bandit per HierState bucket
      // (root + per-branch). Render each as its own line — the legend
      // shows the bucket key plus the currently-leading candidate id so
      // an operator can see at a glance which level has converged.
      var hbuckets = state.hierarchical.buckets || [];
      for (var b = 0; b < hbuckets.length; b++) {
        var bucket = hbuckets[b];
        var label = bucket.key;
        if (bucket.currentLeader) {
          label += ' [' + bucket.currentLeader + ']';
        }
        samples.push({
          name: label,
          mean: bucket.leaderMean || 0,
          trials: bucket.totalRounds || 0
        });
      }
    } else {
      var byId = {};
      var cands = state.candidates || [];
      for (var i = 0; i < cands.length; i++) {
        var c = cands[i];
        if (!byId[c.id]) byId[c.id] = { trials: 0, cum: 0 };
        byId[c.id].trials += c.trials || 0;
        byId[c.id].cum += c.cumulativeReward || 0;
      }
      for (var j = 0; j < META_BANDIT_NAMES.length; j++) {
        var name = META_BANDIT_NAMES[j];
        var agg = byId[name];
        if (agg && agg.trials > 1e-9) {
          samples.push({ name: name, mean: agg.cum / agg.trials, trials: agg.trials });
        } else if (agg) {
          samples.push({ name: name, mean: 0, trials: 0 });
        }
      }
      for (var k in byId) {
        if (META_BANDIT_NAMES.indexOf(k) === -1) {
          var u = byId[k];
          samples.push({
            name: k,
            mean: u.trials > 1e-9 ? u.cum / u.trials : 0,
            trials: u.trials
          });
        }
      }
    }

    chart.push(samples, state.serverNow || Date.now());
  }

  // ----------------------------------------------------------------- //
  // Poll loop                                                         //
  // ----------------------------------------------------------------- //

  function setPollBar(active) {
    var bar = $('poll-bar');
    if (!bar) return;
    bar.classList.toggle('is-active', !!active);
  }

  function bootstrap() {
    var svg = $('rewardChart');
    var legend = $('rewardLegend');
    var chart = new window.SyntraChart.RewardChart(svg, legend);

    var lastMode = null;
    var lastCapsule = null;

    function resetCharts() {
      chart.clear();
      lastMode = null;
      lastCapsule = null;
      // Clear sparkline svgs by wiping the published grid contents.
      var grid = $('publishedGrid');
      if (grid) {
        var cells = grid.querySelectorAll('.published-cell');
        for (var c = 0; c < cells.length; c++) grid.removeChild(cells[c]);
      }
      sparkSvgs = {};
    }

    function tick() {
      var path = selectedCapsule;
      var url = '/api/state';
      if (path) url += '?capsule=' + encodeURIComponent(path);
      setPollBar(true);
      fetch(url, { cache: 'no-store' })
        .then(function (r) { return r.json(); })
        .then(function (state) {
          if (state.scoringMode !== lastMode || state.capsulePath !== lastCapsule) {
            chart.clear();
            lastMode = state.scoringMode;
            lastCapsule = state.capsulePath;
          }

          setHeader(state);
          setChartChrome(state);
          pushChartSamples(chart, state);
          renderDistribution(state);
          renderFeed(state);
          renderPublished(state);
        })
        .catch(function (err) {
          $('headerMeta').textContent = 'dashboard error: ' + err;
        })
        .then(function () { setPollBar(false); });
    }

    function onSelectChange(e) {
      var path = e.target.value || null;
      if (!path) return;
      if (path === selectedCapsule) return;
      selectedCapsule = path;
      setCapsuleHash(path);
      resetCharts();
      tick();
    }

    function onHashChange() {
      var hashPath = capsuleFromHash();
      if (hashPath && hashPath !== selectedCapsule && capsuleIndex[hashPath]) {
        selectedCapsule = hashPath;
        var sel = $('capsule-select');
        if (sel) sel.value = hashPath;
        resetCharts();
        tick();
      }
    }

    // 1. Load capsules first so we know what's available.
    loadCapsules().then(function (list) {
      var hashPath = capsuleFromHash();
      if (hashPath && capsuleIndex[hashPath]) {
        selectedCapsule = hashPath;
      } else if (list.length) {
        // The spec says "first capsule" — /admin/capsules sorts by path.
        selectedCapsule = list[0].path;
        setCapsuleHash(selectedCapsule);
      }
      var sel = $('capsule-select');
      if (sel) {
        if (selectedCapsule) sel.value = selectedCapsule;
        sel.addEventListener('change', onSelectChange);
      }
      window.addEventListener('hashchange', onHashChange);

      tick();
      setInterval(tick, POLL_INTERVAL_MS);
      // Refresh the capsule list every minute so newly installed
      // capsules show up without a page reload.
      setInterval(loadCapsules, 60000);
    });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bootstrap);
  } else {
    bootstrap();
  }
})();
