/* Hand-rolled SVG line chart for the Syntra dashboard.
 *
 * The chart owns one ring buffer per candidate (or one for the shared-state
 * line). Each /api/state poll calls `push(samples)` with the current means;
 * `render()` rebuilds path strings and the axis. No external dependencies,
 * no canvas.
 *
 * Coordinate space is the SVG viewBox: 1000x320. Margins inside reserve
 * room for axis labels.
 */
(function () {
  'use strict';

  // Stable colour palette — algorithm name -> hex. Names match
  // Lang/src/meta_bandit.rs CandidateId variants plus the synthetic
  // "SharedStateLinUcb" line for shared-state capsules.
  var PALETTE = {
    Thompson:           '#5eead4', // teal
    Ucb:                '#c4b5fd', // lavender
    EpsilonGreedy:      '#fbbf24', // amber
    Weighted:           '#fda4af', // rose
    Greedy:             '#94a3b8', // slate
    LinUcb:             '#bef264', // lime
    LinTs:              '#7dd3fc', // sky
    SharedStateLinUcb:  '#22d3ee'  // accent cyan
  };
  var FALLBACK = '#a0a0a0';

  // Chart geometry inside the 1000x320 viewBox.
  var VB_W = 1000;
  var VB_H = 320;
  var M = { top: 12, right: 14, bottom: 28, left: 36 };
  var PLOT_W = VB_W - M.left - M.right;
  var PLOT_H = VB_H - M.top - M.bottom;

  var BUFFER_LEN = 200;        // samples kept per series
  var SAMPLE_STEP_MS = 2000;   // /api/state polling cadence
  var WINDOW_MS = 5 * 60 * 1000;

  // X major ticks every 30s across the 5min window.
  var X_TICKS = [-300, -270, -240, -210, -180, -150, -120, -90, -60, -30, 0];

  function colourFor(name) {
    return PALETTE[name] || FALLBACK;
  }

  function el(tag, attrs) {
    var node = document.createElementNS('http://www.w3.org/2000/svg', tag);
    if (attrs) {
      for (var k in attrs) {
        if (Object.prototype.hasOwnProperty.call(attrs, k)) {
          node.setAttribute(k, attrs[k]);
        }
      }
    }
    return node;
  }

  function clamp01(v) {
    if (!isFinite(v)) return 0;
    if (v < 0) return 0;
    if (v > 1) return 1;
    return v;
  }

  function xForOffsetSec(offsetSec) {
    // offsetSec runs from -300 (oldest) to 0 (now), maps to [0, PLOT_W].
    var t = (offsetSec + 300) / 300; // 0..1
    return M.left + t * PLOT_W;
  }

  function yForReward(r) {
    return M.top + (1 - clamp01(r)) * PLOT_H;
  }

  function RewardChart(svgRoot, legendRoot) {
    this.svg = svgRoot;
    this.legend = legendRoot;
    this.series = {};          // name -> { buffer: [{t, v}], colour }
    this.hidden = {};          // name -> bool
    this.lastPushAt = 0;
    this._buildScaffold();
  }

  RewardChart.prototype._buildScaffold = function () {
    while (this.svg.firstChild) this.svg.removeChild(this.svg.firstChild);

    // Gridline at y=0.5
    this.grid05 = el('line', {
      x1: M.left, x2: VB_W - M.right,
      y1: yForReward(0.5), y2: yForReward(0.5),
      class: 'grid-line'
    });
    this.svg.appendChild(this.grid05);

    // Axes
    this.svg.appendChild(el('line', {
      x1: M.left, x2: M.left, y1: M.top, y2: VB_H - M.bottom, class: 'axis-line'
    }));
    this.svg.appendChild(el('line', {
      x1: M.left, x2: VB_W - M.right, y1: VB_H - M.bottom, y2: VB_H - M.bottom, class: 'axis-line'
    }));

    // Y axis ticks at 0, 0.25, 0.5, 0.75, 1.0
    var yTicks = [0, 0.25, 0.5, 0.75, 1.0];
    for (var i = 0; i < yTicks.length; i++) {
      var y = yForReward(yTicks[i]);
      var t = el('text', {
        x: M.left - 6, y: y + 3,
        'text-anchor': 'end',
        class: 'axis-label'
      });
      t.textContent = yTicks[i].toFixed(2);
      this.svg.appendChild(t);
    }

    // X axis ticks every 30s
    for (var j = 0; j < X_TICKS.length; j++) {
      var sec = X_TICKS[j];
      var x = xForOffsetSec(sec);
      var label;
      if (sec === 0) label = 'now';
      else {
        var m = Math.floor(-sec / 60);
        var s = (-sec) % 60;
        label = '-' + m + (s ? ':' + (s < 10 ? '0' + s : s) : '') + 'm';
      }
      var tx = el('text', {
        x: x, y: VB_H - M.bottom + 14,
        'text-anchor': 'middle',
        class: 'axis-label'
      });
      tx.textContent = label;
      this.svg.appendChild(tx);
    }

    // Series layer — paths get appended/updated here.
    this.seriesLayer = el('g', { class: 'series-layer' });
    this.svg.appendChild(this.seriesLayer);
  };

  /**
   * Push a fresh set of samples and re-render.
   * `samples` is an array of { name, mean, trials } objects.
   */
  RewardChart.prototype.push = function (samples, nowMs) {
    var now = typeof nowMs === 'number' ? nowMs : Date.now();
    this.lastPushAt = now;

    // Pre-fill any new series, append fresh sample.
    var seen = {};
    for (var i = 0; i < samples.length; i++) {
      var s = samples[i];
      seen[s.name] = true;
      if (!this.series[s.name]) {
        this.series[s.name] = {
          buffer: [],
          colour: colourFor(s.name),
          trials: 0
        };
      }
      var entry = this.series[s.name];
      entry.trials = s.trials;
      entry.buffer.push({ t: now, v: s.mean });
      if (entry.buffer.length > BUFFER_LEN) {
        entry.buffer.shift();
      }
    }

    // Drop series that disappeared (rare: only on scoring-mode swap).
    for (var name in this.series) {
      if (!seen[name]) delete this.series[name];
    }

    this._render(now);
    this._renderLegend();
  };

  RewardChart.prototype._render = function (now) {
    var leader = this._leaderName();

    // Walk existing paths; create/update/remove.
    var existing = {};
    var nodes = this.seriesLayer.querySelectorAll('path');
    for (var i = 0; i < nodes.length; i++) {
      existing[nodes[i].getAttribute('data-name')] = nodes[i];
    }

    for (var name in this.series) {
      var path = existing[name];
      if (!path) {
        path = el('path', {
          'data-name': name,
          class: 'series-path',
          stroke: this.series[name].colour
        });
        this.seriesLayer.appendChild(path);
      } else {
        delete existing[name];
      }
      var d = this._pathFor(this.series[name].buffer, now);
      path.setAttribute('d', d || '');
      path.setAttribute('stroke', this.series[name].colour);

      var cls = 'series-path';
      if (name === leader) cls += ' leading';
      if (this.hidden[name]) cls += ' hidden';
      path.setAttribute('class', cls);
    }

    // Anything left in `existing` no longer has data — remove.
    for (var stale in existing) {
      this.seriesLayer.removeChild(existing[stale]);
    }
  };

  RewardChart.prototype._pathFor = function (buffer, now) {
    if (!buffer || buffer.length === 0) return '';
    var d = '';
    var drew = 0;
    for (var i = 0; i < buffer.length; i++) {
      var pt = buffer[i];
      var ageMs = now - pt.t;
      if (ageMs > WINDOW_MS) continue;
      var offsetSec = -ageMs / 1000;
      if (offsetSec < -300) offsetSec = -300;
      if (offsetSec > 0) offsetSec = 0;
      var x = xForOffsetSec(offsetSec);
      var y = yForReward(pt.v);
      d += (drew === 0 ? 'M' : 'L') + x.toFixed(1) + ' ' + y.toFixed(1) + ' ';
      drew++;
    }
    return d;
  };

  RewardChart.prototype._leaderName = function () {
    var best = null;
    var bestVal = -Infinity;
    for (var name in this.series) {
      var b = this.series[name].buffer;
      if (!b || b.length === 0) continue;
      var v = b[b.length - 1].v;
      if (v > bestVal) { bestVal = v; best = name; }
    }
    return best;
  };

  RewardChart.prototype._renderLegend = function () {
    if (!this.legend) return;
    var self = this;
    var names = Object.keys(this.series).sort();
    var existing = {};
    var nodes = this.legend.querySelectorAll('.legend-item');
    for (var i = 0; i < nodes.length; i++) {
      existing[nodes[i].getAttribute('data-name')] = nodes[i];
    }
    for (var j = 0; j < names.length; j++) {
      var name = names[j];
      var entry = this.series[name];
      var item = existing[name];
      if (!item) {
        item = document.createElement('span');
        item.className = 'legend-item';
        item.setAttribute('data-name', name);
        var swatch = document.createElement('span');
        swatch.className = 'swatch';
        item.appendChild(swatch);
        var label = document.createElement('span');
        label.className = 'label';
        item.appendChild(label);
        var trials = document.createElement('span');
        trials.className = 'trials';
        item.appendChild(trials);
        item.addEventListener('click', (function (n) {
          return function () { self._toggle(n); };
        })(name));
        this.legend.appendChild(item);
      } else {
        delete existing[name];
      }
      item.querySelector('.swatch').style.background = entry.colour;
      item.querySelector('.label').textContent = name;
      item.querySelector('.trials').textContent = '· ' + Math.round(entry.trials) + ' trials';
      item.classList.toggle('is-off', !!this.hidden[name]);
    }
    for (var stale in existing) {
      this.legend.removeChild(existing[stale]);
    }
  };

  RewardChart.prototype._toggle = function (name) {
    this.hidden[name] = !this.hidden[name];
    var path = this.seriesLayer.querySelector('path[data-name="' + name + '"]');
    if (path) path.classList.toggle('hidden', !!this.hidden[name]);
    this._renderLegend();
  };

  RewardChart.prototype.clear = function () {
    this.series = {};
    this.hidden = {};
    this._buildScaffold();
    if (this.legend) this.legend.innerHTML = '';
  };

  // ----------------------------------------------------------------- //
  // Sparkline (Region 5)                                              //
  // ----------------------------------------------------------------- //
  //
  // renderSparkline(svgEl, values) — paints a polyline of up to 60
  // numeric samples into a fixed 100x24 viewBox. The svg is expected to
  // already declare `viewBox="0 0 100 24"` and `preserveAspectRatio="none"`.
  // Non-numeric samples (strings, booleans, null) are skipped — useful
  // for keys like "marker" that publish text instead of numbers.

  function renderSparkline(svg, values) {
    while (svg.firstChild) svg.removeChild(svg.firstChild);
    if (!values || !values.length) return;
    var nums = [];
    for (var i = 0; i < values.length; i++) {
      var v = values[i];
      if (typeof v === 'number' && isFinite(v)) nums.push(v);
    }
    if (nums.length < 2) return;
    var lo = nums[0];
    var hi = nums[0];
    for (var j = 1; j < nums.length; j++) {
      if (nums[j] < lo) lo = nums[j];
      if (nums[j] > hi) hi = nums[j];
    }
    var span = hi - lo;
    if (span < 1e-9) span = 1;  // flat line — pin to mid-band
    var n = nums.length;
    var d = '';
    for (var k = 0; k < n; k++) {
      var x = n === 1 ? 50 : (k / (n - 1)) * 100;
      var y = 22 - ((nums[k] - lo) / span) * 20;  // 2px padding top, 2px bottom
      d += (k === 0 ? 'M' : 'L') + x.toFixed(2) + ' ' + y.toFixed(2) + ' ';
    }
    var path = el('path', { d: d, class: 'sparkline-path' });
    svg.appendChild(path);
  }

  window.SyntraChart = {
    RewardChart: RewardChart,
    colourFor: colourFor,
    renderSparkline: renderSparkline
  };
})();
