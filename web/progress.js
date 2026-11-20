import { esc, num } from "./schedule.js";

export const scoreLevels = { hard: "Hard", medium: "Medium", soft: "Soft" };
export function progressLevels(mode = "auto", score) {
  if (mode === "all") return Object.keys(scoreLevels);
  if (Object.hasOwn(scoreLevels, mode)) return [mode];
  if (!score) return [];
  return integer(score.hard) === 0n ? ["medium", "soft"] : ["hard"];
}
const integer = (value) => BigInt(value ?? 0);
const format = (value) => value.toLocaleString("en-US");
const ceil = (a, b) => -floor(-a, b);
const floor = (a, b) => a >= 0n ? a / b : -((-a + b - 1n) / b);

function scoreTicks(values) {
  let low = values.reduce((a, b) => a < b ? a : b);
  let high = values.reduce((a, b) => a > b ? a : b);
  if (low === high) {
    const padding = (low < 0n ? -low : low) / 50n || 1n;
    low -= padding;
    high += padding;
    if (values[0] <= 0n && high > 0n) high = 0n;
  }
  const rough = (high - low + 3n) / 4n || 1n;
  const magnitude = 10n ** BigInt(rough.toString().length - 1);
  const step = [1n, 2n, 5n, 10n].map((v) => v * magnitude).find((v) => v >= rough);
  const bottom = floor(low, step) * step;
  const top = ceil(high, step) * step;
  const ticks = [];
  for (let value = bottom; value <= top; value += step) ticks.push(value);
  return { bottom, top, ticks };
}

function timeTicks(maximum, count) {
  const rough = maximum / count;
  const magnitude = 10 ** Math.floor(Math.log10(rough));
  const step = [1, 2, 5, 10].map((v) => v * magnitude).find((v) => v >= rough);
  const ticks = [];
  for (let i = 0; i * step <= maximum + step / 1000; i++) ticks.push(i * step);
  if (maximum - ticks.at(-1) > step * .35) ticks.push(maximum);
  return ticks;
}

function compactTick(value) {
  const magnitude = value < 0n ? -value : value;
  if (magnitude < 10_000n) return format(value);
  return Number(value).toExponential(1).replace("e+", "e");
}

function chartAxis(level, samples, compact) {
  const domain = scoreTicks(samples.map((point) => integer(point.all[level])));
  let base = 0n, labels = domain.ticks.map((value) => compact ? compactTick(value) : format(value));
  // Offsets keep nearby large integers distinct even in a narrow multi-axis plot.
  if (compact && new Set(labels).size !== labels.length) {
    base = domain.top;
    labels = domain.ticks.map((value) => compactTick(value - base));
  }
  return { level, domain, base, labels, width: Math.max(48, ...labels.map((value) => value.length * 6.2 + 14)) };
}

export function progressChart(points, { level = "soft", levels, width = 760, elapsedMs = 0, startMs = 0 } = {}) {
  const selected = levels?.length ? levels.filter((key) => Object.hasOwn(scoreLevels, key)) :
    [Object.hasOwn(scoreLevels, level) ? level : "soft"];
  const samples = points.filter((point) => point.bestScore).map((point) => ({
    time: Math.max(0, num(point.elapsedMs) / 1000),
    all: point.bestScore,
  })).sort((a, b) => a.time - b.time);
  if (!samples.length) return '<p class="trend-empty">等待进度采样后显示分数趋势。</p>';
  const chartWidth = Math.max(280, width);
  let axes = selected.map((key) => chartAxis(key, samples, false));
  if (axes.reduce((sum, axis) => sum + axis.width + 8, 18) > chartWidth - 100)
    axes = selected.map((key) => chartAxis(key, samples, true));
  const left = Math.max(58, axes[0].width) + 2;
  const right = chartWidth - 12 - axes.slice(1).reduce((sum, axis) => sum + axis.width + 8, 0);
  const top = 28, bottom = 138, height = 180;
  const minimum = Math.max(0, num(startMs) / 1000);
  const maximum = Math.max(.1, num(elapsedMs) / 1000, ...samples.map((point) => point.time));
  const x = (time) => left + (time - minimum) / Math.max(.1, maximum - minimum) * (right - left);
  const xTicks = timeTicks(Math.max(.1, maximum - minimum), Math.max(2, Math.floor((right - left) / 85))).map((offset) => {
    const value = minimum + offset;
    return `<g class="trend-x-tick" data-value="${value}"><line x1="${x(value)}" x2="${x(value)}" y1="${bottom}" y2="${bottom + 4}"/><text x="${x(value)}" y="${bottom + 17}" text-anchor="${offset === 0 ? "start" : value === maximum ? "end" : "middle"}">${Number(value.toFixed(2))}</text></g>`;
  }).join("");
  let nextRight = right;
  const axisMarkup = axes.map(({ level: key, domain, base, labels, width: axisWidth }, index) => {
    const position = index === 0 ? left : nextRight;
    if (index) nextRight += axisWidth + 8;
    const direction = index === 0 ? -1 : 1, anchor = index === 0 ? "end" : "start";
    const y = (value) => bottom - Number(value - domain.bottom) / Number(domain.top - domain.bottom) * (bottom - top);
    return `<g class="trend-y-axis" data-score-axis="${key}"><line x1="${position}" x2="${position}" y1="${top}" y2="${bottom}"/><text class="trend-axis-title" x="${position + direction * 7}" y="15" text-anchor="${anchor}">${scoreLevels[key]}${base ? " Δ" : ""}</text>${domain.ticks.map((value, i) => `<g class="trend-y-tick" data-value="${value}"><line x1="${position}" x2="${position + direction * 4}" y1="${y(value)}" y2="${y(value)}"/><text x="${position + direction * 7}" y="${y(value) + 3}" text-anchor="${anchor}">${labels[i]}</text></g>`).join("")}</g>`;
  }).join("");
  const curves = axes.map(({ level: key, domain }, index) => {
    // Integer subtraction retains small changes above Number.MAX_SAFE_INTEGER.
    const y = (point) => bottom - Number(integer(point.all[key]) - domain.bottom) / Number(domain.top - domain.bottom) * (bottom - top);
    const curve = samples.map((point, i) => i === 0 ? `M ${x(point.time)} ${y(point)}` : `H ${x(point.time)} V ${y(point)}`).join(" ");
    const dots = samples.filter((point, i) => samples.length < 32 || i === 0 || i === samples.length - 1 || integer(point.all[key]) !== integer(samples[i - 1].all[key])).map((point) => {
      const description = `耗时 ${Number(point.time.toFixed(2))} 秒；` + Object.entries(scoreLevels).map(([name, title]) => `${title} ${format(integer(point.all[name]))}`).join(" / ");
      const radius = selected.length > 1 ? selected.length - index + 1 : samples.length === 1 ? 3 : 2;
      return `<circle class="trend-point" cx="${x(point.time)}" cy="${y(point)}" r="${radius}" tabindex="0" aria-label="${esc(description)}"><title>${esc(description)}</title></circle>`;
    }).join("");
    return `<g class="trend-series" data-score-level="${key}"><path class="trend-curve" d="${curve}"/>${dots}</g>`;
  }).join("");
  const grid = [0, .25, .5, .75, 1].map((fraction) => `<line class="trend-grid" x1="${left}" x2="${right}" y1="${top + fraction * (bottom - top)}" y2="${top + fraction * (bottom - top)}"/>`).join("");
  const legend = selected.length > 1 || axes.some((axis) => axis.base) ? `<div class="trend-legend">${axes.map(({ level: key, base }) => `<span data-score-legend="${key}"><i aria-hidden="true"></i>${scoreLevels[key]} <strong>${format(integer(samples.at(-1).all[key]))}</strong>${base ? `<small>Δ 基准 ${format(base)}</small>` : ""}</span>`).join("")}</div>` : "";
  const names = selected.map((key) => scoreLevels[key]).join(" / ");
  return legend + `<svg class="progress-chart" data-score-level="${selected.length === 1 ? selected[0] : "multiple"}" data-last-elapsed-ms="${samples.at(-1).time * 1000}" viewBox="0 0 ${chartWidth} ${height}" preserveAspectRatio="none" role="img" aria-label="${names} 分数随求解耗时的变化；各级别独立纵轴"><title>横轴：求解耗时（秒）；纵轴：${names} 各自分数，颜色对应各级别，纵轴刻度独立</title>${grid}${axisMarkup}<line class="trend-axis" x1="${left}" x2="${right}" y1="${bottom}" y2="${bottom}"/>${xTicks}<text class="trend-axis-title" x="${(left + right) / 2}" y="${height - 5}" text-anchor="middle">求解耗时（秒）</text>${curves}</svg>`;
}

export function progressCharts(points, { mode = "auto", score, width = 760, elapsedMs = 0, range = "auto" } = {}) {
  const levels = progressLevels(mode, score);
  if (!levels.length) return '<p class="trend-empty">等待首个有效分数后显示趋势。</p>';
  const displayed = mode === "auto" && levels[0] === "medium" ?
    points.filter((point) => point.bestScore && integer(point.bestScore.hard) === 0n) : points;
  const stage = mode === "auto" ? `<p class="trend-stage">${levels[0] === "hard" ?
    "Hard 尚未归零 · 优先查看硬约束改善" : "Hard = 0 · 查看该阶段的 Medium / Soft 优化"}</p>` : "";
  const ordered = displayed.filter(p => p.bestScore).slice().sort((a, b) => num(a.elapsedMs) - num(b.elapsedMs));
  if (!ordered.length) return stage + '<p class="trend-empty">等待进度采样后显示分数趋势。</p>';
  const primary = levels.at(-1), values = ordered.map(p => integer(p.bestScore[primary]));
  const low = values.reduce((a, b) => a < b ? a : b), high = values.reduce((a, b) => a > b ? a : b);
  let start = 0;
  const endTime = Math.max(num(elapsedMs), num(ordered.at(-1).elapsedMs));
  if (range !== "full" && high > low && ordered.length >= 12) {
    // Keep the complete stage visible; magnify only a long tail compressed by early outliers.
    for (let i = 1; i < ordered.length - 8 && num(ordered[i].elapsedMs) <= endTime * .2; i++) {
      const tail = values.slice(i), tailLow = tail.reduce((a, b) => a < b ? a : b), tailHigh = tail.reduce((a, b) => a > b ? a : b);
      if (tailHigh > tailLow && (tailHigh - tailLow) * 5n < high - low) { start = i; break; }
    }
  }
  let improvements = 0;
  for (let i = 1; i < ordered.length; i++) {
    for (const key of Object.keys(scoreLevels)) {
      const delta = integer(ordered[i].bestScore[key]) - integer(ordered[i - 1].bestScore[key]);
      if (delta) { if (delta > 0n) improvements++; break; }
    }
  }
  const first = format(integer(ordered[0].bestScore[primary])), last = format(integer(ordered.at(-1).bestScore[primary]));
  const summary = `<div class="trend-summary"><strong>${scoreLevels[primary]} ${first} → ${last}</strong><span>${ordered.length} 个采样 · 采样内 ${improvements} 次改善</span></div>`;
  let overview = "";
  if (start) {
    const maxTime = Math.max(.1, endTime / 1000), w = Math.max(280, width), left = 12, right = w - 12;
    const x = point => left + num(point.elapsedMs) / 1000 / maxTime * (right - left);
    const y = point => 37 - Number(integer(point.bestScore[primary]) - low) / Number(high - low) * 30;
    const path = ordered.map((point, i) => i ? `H ${x(point)} V ${y(point)}` : `M ${x(point)} ${y(point)}`).join(' ');
    overview = `<div class="trend-overview-heading"><span>本阶段全程概览 · 0–${Number(maxTime.toFixed(1))} 秒</span><strong>优化细节 ${Number((num(ordered[start].elapsedMs)/1000).toFixed(1))}–${Number(maxTime.toFixed(1))} 秒</strong></div><svg class="trend-overview" viewBox="0 0 ${w} 44" role="img" aria-label="${scoreLevels[primary]} 完整采样概览，阴影为下方放大的时间段"><rect x="${x(ordered[start])}" y="2" width="${right-x(ordered[start])}" height="40" fill="#e5f5fa"/><path d="${path}" fill="none" stroke="#427d91" stroke-width="1.5"/></svg>`;
  }
  return stage + summary + overview + `<div class="trend-charts" data-progress-mode="${esc(mode)}" data-progress-range="${start ? 'detail' : 'full'}">${progressChart(ordered.slice(start), { levels, width, elapsedMs, startMs: start ? ordered[start].elapsedMs : 0 })}</div>`;
}
