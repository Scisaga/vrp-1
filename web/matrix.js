import { list, num, esc, label } from './schedule.js';

const cache = new WeakMap(), UNKNOWN = 4294967294, UNREACHABLE = 4294967295;
const readiness = new WeakMap();
export function matrixReady(request) {
  const p = request?.problem, n = list(p?.locations).length;
  if (!n || !list(p.matrices).length) return false;
  const used = new Set(list(p.agents).map(a => num(a.matrix)));
  if (!used.size) used.add(0);
  for (const index of used) {
    const matrix = list(p.matrices)[index];
    if (!matrix) return false;
    const previous = readiness.get(matrix);
    if (previous?.n === n) { if (!previous.ready) return false; continue; }
    let ready = true;
    try {
      const b = matrixBytes(matrix), size = n * n;
      ready = b.durations.byteLength === size * 4 && b.distances.byteLength === size * 4 && (!b.sources.length || b.sources.length === size);
      for (let k = 0; ready && k < size; k++) {
        const duration = b.durations.getUint32(k * 4, true), distance = b.distances.getUint32(k * 4, true);
        ready = duration !== UNKNOWN && distance !== UNKNOWN && (duration === UNREACHABLE) === (distance === UNREACHABLE) && (!b.sources.length || b.sources[k] <= 3) && (k % (n + 1) !== 0 || (duration === 0 && distance === 0));
      }
    } catch { ready = false; }
    readiness.set(matrix, { n, ready });
    if (!ready) return false;
  }
  return true;
}
export function matrixBytes(matrix) {
  if (!cache.has(matrix)) {
    const decode = s => Uint8Array.from(atob(s || ''), c => c.charCodeAt(0));
    cache.set(matrix, { durations: new DataView(decode(matrix.durations).buffer), distances: new DataView(decode(matrix.distances).buffer), sources: decode(matrix.sources) });
  }
  return cache.get(matrix);
}
function source(c, value, from, to) {
  if (from === to) return '同一地点';
  if (value.duration === UNREACHABLE) return '不可达';
  if (value.duration === UNKNOWN) return '待估算';
  return value.source === 3 ? '几何估算' : value.source === 2 ? '模型估算' : c.scene.scenario.origin?.historicalValuesUnverified ? '历史导入' : value.source === 1 ? '输入缓存' : '输入值';
}
export function matrixValue(matrix, n, from, to) {
  if (from === to) return { duration: 0, distance: 0, source: 0 };
  if (!matrix) return { duration: UNKNOWN, distance: UNKNOWN, source: 3 };
  const b = matrixBytes(matrix), k = from * n + to;
  if ((k + 1) * 4 > b.durations.byteLength || (k + 1) * 4 > b.distances.byteLength) return { duration: UNKNOWN, distance: UNKNOWN, source: 3 };
  return { duration: b.durations.getUint32(k * 4, true), distance: b.distances.getUint32(k * 4, true), source: b.sources[k] || 0 };
}
const formatted = (value, metric) => value >= UNKNOWN ? '—' : metric === 'distance' ? `${(value / 1000).toLocaleString('zh-CN', { maximumFractionDigits: 2 })} km` : `${(value / 60).toLocaleString('zh-CN', { maximumFractionDigits: 1 })} 分`;
function related(c, index) {
  const p = c.draft.request.problem, d = c.draft.display;
  return [...list(p.tickets).filter(t => num(t.location) === index).map(t => label(d, 'tickets', t.id)),
    ...list(p.agents).flatMap(a => [num(a.startLocation) === index ? `${label(d, 'agents', a.id)} · 出发` : '', num(a.endLocation) === index ? `${label(d, 'agents', a.id)} · 返回` : '']).filter(Boolean)];
}
export function matrixPanel(c) {
  const p = c.draft.request.problem, n = list(p.locations).length, matrices = list(p.matrices), metric = c.matrixMetric || 'duration';
  c.matrixProfile = Math.min(c.matrixProfile || 0, Math.max(0, matrices.length - 1));
  const options = selected => '<option value="">全部地点</option>' + list(p.locations).map((l, i) => `<option value="${i}" ${String(selected) === String(i) ? 'selected' : ''}>${esc(label(c.draft.display, 'locations', l.id))}</option>`).join('');
  return `<section class="panel matrix-panel"><div class="panel-head"><div><h2>在途矩阵</h2><p class="hint">${n} 个地点 · 行为起点，列为终点 · 颜色越深，${metric === 'duration' ? '时间越长' : '距离越远'}</p></div><div class="actions">${!c.op ? '<button data-action="geo-plan" class="primary">生成矩阵并规划</button>' : ''}<div class="segmented"><button data-matrix-metric="duration" class="${metric === 'duration' ? 'selected' : ''}">行驶时间</button><button data-matrix-metric="distance" class="${metric === 'distance' ? 'selected' : ''}">行驶距离</button></div></div></div><p class="matrix-ready-status ${matrixReady(c.draft.request) ? 'ready' : 'pending'}" role="status">${matrixReady(c.draft.request) ? '在途矩阵已就绪，可提交求解。' : '在途矩阵未就绪。先生成并固化完整矩阵，再开始求解。'}</p><div class="matrix-toolbar"><label>车型 <select id="matrix-profile">${matrices.length ? matrices.map((m, i) => `<option value="${i}" ${i === c.matrixProfile ? 'selected' : ''}>${esc(m.profile || '默认')}</option>`).join('') : '<option value="0">默认</option>'}</select></label><label>起点 <select id="matrix-from">${options(c.matrixFrom)}</select></label><label>终点 <select id="matrix-to">${options(c.matrixTo)}</select></label><label>缩放 <select id="matrix-zoom">${[1, 2, 4].map(v => `<option value="${v}" ${v === (c.matrixZoom || 1) ? 'selected' : ''}>${v === 1 ? '总览' : v + '×'}</option>`).join('')}</select></label></div><div class="matrix-layout"><div class="matrix-visual"><div class="matrix-scroll" role="region" aria-label="在途矩阵热力图"><canvas id="matrix-canvas" tabindex="0" role="img" aria-label="在途矩阵热力图，方向键选择路段，Enter 查看关联对象"></canvas></div><div class="matrix-legend"><span>短</span><span class="matrix-gradient"></span><span id="matrix-max"></span><span class="matrix-swatch zero"></span>同点<span class="matrix-swatch unknown"></span>待估算<span class="matrix-swatch unreachable"></span>不可达</div><p class="hint matrix-hint">悬停查看数值，点击固定路段；颜色刻度按完整车型矩阵固定，筛选不改变刻度。</p></div><aside class="matrix-detail" aria-live="polite"><div id="matrix-cell-detail"></div></aside></div></section>`;
}
export function mountMatrix(c, { render, locate }) {
  const canvas = document.getElementById('matrix-canvas'); if (!canvas) return;
  const p = c.draft.request.problem, locations = list(p.locations), n = locations.length, matrix = list(p.matrices)[c.matrixProfile || 0], metric = c.matrixMetric || 'duration';
  const indices = value => value === undefined || value === '' || !locations[Number(value)] ? locations.map((_, i) => i) : [Number(value)];
  const rows = indices(c.matrixFrom), cols = indices(c.matrixTo), pad = 68;
  const box = canvas.parentElement, available = Math.max(220, box.clientWidth - pad - 12), side = Math.max(200, Math.min(available, 610));
  const cell = Math.max(1, Math.min(60, side / Math.max(rows.length, cols.length, 1))) * (c.matrixZoom || 1);
  const width = pad + Math.max(1, cols.length) * cell + 6, height = pad + Math.max(1, rows.length) * cell + 6;
  canvas.width = Math.ceil(width * devicePixelRatio); canvas.height = Math.ceil(height * devicePixelRatio);
  canvas.style.width = `${width}px`; canvas.style.height = `${height}px`;
  const ctx = canvas.getContext('2d'); ctx.scale(devicePixelRatio, devicePixelRatio);
  let max = 0;
  // Keep the scale stable when selecting a submatrix. Ignore sentinel values.
  if (matrix) {
    const values = matrixBytes(matrix)[metric === 'duration' ? 'durations' : 'distances'];
    for (let k = 0; k + 4 <= values.byteLength; k += 4) { const v = values.getUint32(k, true); if (v < UNKNOWN) max = Math.max(max, v); }
  }
  document.getElementById('matrix-max').textContent = formatted(max, metric);
  const raster = document.createElement('canvas'); raster.width = Math.max(1, cols.length); raster.height = Math.max(1, rows.length);
  const rasterContext = raster.getContext('2d'), pixels = rasterContext.createImageData(raster.width, raster.height);
  for (let r = 0; r < rows.length; r++) for (let j = 0; j < cols.length; j++) {
    const edge = matrixValue(matrix, n, rows[r], cols[j]), value = edge[metric], offset = (r * cols.length + j) * 4;
    let color;
    if (rows[r] === cols[j]) color = [246, 248, 251];
    else if (value === UNREACHABLE) color = [180, 55, 68];
    else if (value === UNKNOWN) color = [151, 160, 171];
    else { const t = max ? value / max : 0; color = [Math.round(228 - 228 * t), Math.round(247 - 145 * t), Math.round(251 - 125 * t)]; }
    pixels.data.set([...color, 255], offset);
  }
  rasterContext.putImageData(pixels, 0, 0);
  const rowOf = value => Math.max(0, rows.indexOf(value)), colOf = value => Math.max(0, cols.indexOf(value));
  let selected = c.matrixSelected ? [rowOf(c.matrixSelected.from), colOf(c.matrixSelected.to)] : [0, cols.length > 1 ? 1 : 0];
  function draw(highlight = selected) {
    ctx.clearRect(0, 0, width, height); ctx.fillStyle = '#fff'; ctx.fillRect(0, 0, width, height);
    ctx.imageSmoothingEnabled = false; ctx.drawImage(raster, pad, pad, cols.length * cell, rows.length * cell);
    ctx.font = '10px system-ui'; ctx.fillStyle = '#34566c';
    const step = Math.max(1, Math.ceil(18 / cell));
    for (let r = 0; r < rows.length; r += step) { ctx.textAlign = 'right'; ctx.fillText(String(rows[r] + 1).padStart(3, '0'), pad - 9, pad + (r + .5) * cell + 3); }
    for (let j = 0; j < cols.length; j += step) { ctx.save(); ctx.translate(pad + (j + .5) * cell, pad - 9); ctx.rotate(-Math.PI / 3); ctx.textAlign = 'left'; ctx.fillText(String(cols[j] + 1).padStart(3, '0'), 0, 0); ctx.restore(); }
    ctx.textAlign = 'left'; ctx.fillText('起点 ↓', 4, pad - 8); ctx.fillText('终点 →', pad, 13);
    if (cell >= 8 && rows.length * cols.length <= 30000) {
      ctx.strokeStyle = '#ffffff66'; ctx.lineWidth = .5;
      ctx.beginPath(); for (let r = 0; r <= rows.length; r++) { ctx.moveTo(pad, pad + r * cell); ctx.lineTo(pad + cols.length * cell, pad + r * cell); }
      for (let j = 0; j <= cols.length; j++) { ctx.moveTo(pad + j * cell, pad); ctx.lineTo(pad + j * cell, pad + rows.length * cell); } ctx.stroke();
      ctx.strokeStyle = '#ffffffaa'; ctx.lineWidth = 1;
      for (let r = 0; r < rows.length; r++) for (let j = 0; j < cols.length; j++) {
        const value = matrixValue(matrix, n, rows[r], cols[j])[metric];
        if (value < UNKNOWN) continue;
        const x = pad + j * cell, y = pad + r * cell;
        ctx.beginPath(); ctx.moveTo(x + 2, y + cell - 2); ctx.lineTo(x + cell - 2, y + 2);
        if (value === UNREACHABLE) { ctx.moveTo(x + 2, y + 2); ctx.lineTo(x + cell - 2, y + cell - 2); }
        ctx.stroke();
      }
    }
    if (rows.length && cols.length) {
      const [r, j] = highlight;
      ctx.fillStyle = '#ff992f33'; ctx.fillRect(pad, pad + r * cell, cols.length * cell, cell); ctx.fillRect(pad + j * cell, pad, cell, rows.length * cell);
      ctx.strokeStyle = '#d9740e'; ctx.lineWidth = 2; ctx.strokeRect(pad + j * cell, pad + r * cell, Math.max(2, cell), Math.max(2, cell));
    }
  }
  function detail(r, j) {
    if (!rows.length || !cols.length) { document.getElementById('matrix-cell-detail').textContent = '先为工单或执行单元添加地点。'; return; }
    const from = rows[r], to = cols[j], edge = matrixValue(matrix, n, from, to), origin = locations[from], destination = locations[to];
    const info = (index, location, title) => { const names = related(c, index); return `<div class="matrix-endpoint"><small>${title} · ${String(index + 1).padStart(3, '0')}</small><strong>${esc(label(c.draft.display, 'locations', location.id))}</strong><span>${esc(names.slice(0, 5).join('、') || '暂无工单或执行单元引用')}${names.length > 5 ? ` 等 ${names.length} 项` : ''}</span><button data-matrix-locate="${index}">查看地点与关联对象</button></div>`; };
    document.getElementById('matrix-cell-detail').innerHTML = `${info(from, origin, '起点')}<div class="matrix-arrow">↓</div>${info(to, destination, '终点')}<dl class="matrix-values"><dt>行驶时间</dt><dd>${formatted(edge.duration, 'duration')}${edge.duration < UNKNOWN ? `<small>${edge.duration.toLocaleString()} 秒</small>` : ''}</dd><dt>行驶距离</dt><dd>${formatted(edge.distance, 'distance')}${edge.distance < UNKNOWN ? `<small>${edge.distance.toLocaleString()} 米</small>` : ''}</dd><dt>来源</dt><dd>${source(c, edge, from, to)}</dd></dl>${edge.source === 3 && edge.duration < UNKNOWN ? '<p class="hint">显示未加排班余量的估算值。</p>' : ''}`;
  }
  const hit = event => { const bounds = canvas.getBoundingClientRect(), j = Math.floor((event.clientX - bounds.left - pad) / cell), r = Math.floor((event.clientY - bounds.top - pad) / cell); return r >= 0 && r < rows.length && j >= 0 && j < cols.length ? [r, j] : null; };
  canvas.onpointermove = event => { const pair = hit(event); if (pair) { draw(pair); detail(...pair); } };
  canvas.onpointerleave = () => { draw(); detail(...selected); };
  canvas.onclick = event => { const pair = hit(event); if (pair) { selected = pair; c.matrixSelected = { from: rows[pair[0]], to: cols[pair[1]] }; draw(); detail(...selected); } };
  canvas.onkeydown = event => {
    if (!rows.length || !cols.length) return;
    let [r, j] = selected;
    if (event.key === 'ArrowDown') r++; else if (event.key === 'ArrowUp') r--; else if (event.key === 'ArrowRight') j++; else if (event.key === 'ArrowLeft') j--; else if (event.key === 'Enter') { locate(rows[r]); return; } else return;
    event.preventDefault(); selected = [Math.max(0, Math.min(rows.length - 1, r)), Math.max(0, Math.min(cols.length - 1, j))]; c.matrixSelected = { from: rows[selected[0]], to: cols[selected[1]] }; draw(); detail(...selected);
  };
  canvas.closest('.matrix-panel').addEventListener('click', event => {
    const metricButton = event.target.closest('[data-matrix-metric]'), locationButton = event.target.closest('[data-matrix-locate]');
    if (metricButton) { c.matrixMetric = metricButton.dataset.matrixMetric; render(); }
    if (locationButton) locate(Number(locationButton.dataset.matrixLocate));
  });
  document.getElementById('matrix-zoom').onchange = event => { c.matrixZoom = Number(event.target.value); render(); };
  draw(); detail(...selected);
}
