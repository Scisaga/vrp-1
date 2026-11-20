import { list, num, esc, clone, stamp, parseTime, label, tp, constraintNames } from './schedule.js';

const ROW = 32, WINDOW = 70;
const titles = { tickets: '工单', agents: '执行单元', locations: '地点', rules: '约束权重', costs: '费用与排班余量' };
const settings = kind => kind === 'rules' || kind === 'costs';
const types = [['TICKET_TYPE_SERVICE', '服务'], ['TICKET_TYPE_DELIVERY', '配送'], ['TICKET_TYPE_PICKUP', '取件']];
const nameOf = (c, kind, id) => label(c.draft.display, kind, id);
const safeSet = (object, key, value) => Object.defineProperty(object, key, { value, enumerable: true, writable: true, configurable: true });
const integer = (text, optional = false) => {
  if (optional && !String(text).trim()) return undefined;
  if (!/^\d+$/.test(String(text).trim()) || !Number.isSafeInteger(Number(text))) throw Error('请输入非负整数');
  return String(text).trim();
};
const split = (value) => String(value).split(/[,，\n]/).map(s => s.trim()).filter(Boolean);

// A pointer press blurs the edited cell before the destination receives its
// click. Commit immediately, but keep that DOM alive through the whole gesture.
let pointerHeld = false, deferredChange;
document.addEventListener('pointerdown', () => { pointerHeld = true; }, true);
function finishPointer() {
  pointerHeld = false;
  const refresh = deferredChange; deferredChange = null;
  if (refresh) setTimeout(refresh, 0);
}
for (const event of ['pointerup', 'pointercancel', 'click', 'blur'])
  window.addEventListener(event, finishPointer, event !== 'blur');
function renderAfterChange(root, render) {
  if (pointerHeld) deferredChange = () => { if (root.isConnected) render(); };
  else render();
}

// Matrix byte strings are immutable; undo snapshots share these large arrays.
function snapshot(c) {
  const p = c.draft.request.problem;
  const value = clone({ ...c.draft, request: { ...c.draft.request, problem: { ...p, matrices: [] } } });
  value.request.problem.matrices = p.matrices;
  return { draft: value, dirty: c.dirty };
}
export function checkpoint(c) {
  c.sheetUndo ||= [];
  c.sheetUndo.push(snapshot(c));
  if (c.sheetUndo.length > 20) c.sheetUndo.shift();
  c.sheetRedo = [];
}
export function resetSheetHistory(c) {
  c.sheetUndo = []; c.sheetRedo = []; c.sheetPending = new Map(); c.sheetError = '';
}
export function undoSheet(c, redo = false) {
  const source = redo ? c.sheetRedo : c.sheetUndo;
  if (!source?.length) return;
  (redo ? c.sheetUndo : c.sheetRedo).push(snapshot(c));
  Object.assign(c, source.pop());
  c.sheetPending = new Map(); c.sheetError = ''; c.rawDirty = false;
  delete c.rawDraft;
}
export function sheetHasErrors(c) { return !!c.sheetPending?.size; }
function changed(c) { c.dirty = true; c.rawDirty = false; delete c.rawDraft; }
function locked(c, kind, item) {
  if (c.op?.closed) return true;
  return kind === 'tickets' && list(c.op?.tickets).some(t => t.ticketId === item.id && tp(t) >= 3);
}
function readonly(c, kind, row, col) {
  return locked(c, kind, row.v) || (typeof col.readonly === 'function' ? col.readonly(row.v) : col.readonly);
}
function sourceRows(kind, problem, display, options) {
  if (kind === 'rules') return Object.entries(constraintNames).map(([id, name], index) => {
    const weight = problem.weights?.[id] || options.defaults[id] || {};
    return { index, d: {}, v: { id, name, hard: String(weight.hard || 0), medium: String(weight.medium || 0), soft: String(weight.soft || 0),
      setting: id === 'executionConsistency' ? '固定启用' : problem.weights?.[id] ? '自定义' : '默认' } };
  });
  if (kind === 'costs') {
    // A present EstimateBuffer uses literal protobuf values, including omitted
    // zero fields; only an absent message selects the 100/200 defaults.
    const buffer = problem.estimateBuffer || { modelPermille: 100, fallbackPermille: 200 };
    return [
      ['restrictedChargePerHour', '限行费用', problem.costs?.restrictedChargePerHour || '0', '整数金额 / 小时', '按限行执行单元的行驶时间计费；每个执行单元的固定费用在执行单元表维护。'],
      ['modelPermille', '模型估算排班余量', buffer.modelPermille || 0, '‰', '100‰ 表示排班时长增加 10%，不修改矩阵原始值。'],
      ['fallbackPermille', '几何估算排班余量', buffer.fallbackPermille || 0, '‰', '200‰ 表示排班时长增加 20%，不修改矩阵原始值。'],
    ].map(([id, name, value, unit, note], index) => ({ index, d: {}, v: { id, name, value: String(value), unit, note } }));
  }
  return list(problem[kind]).map((v, index) => ({ v: kind === 'tickets' ? { ...v,
    _predecessors: [...new Set(list(problem.precedences).filter(pair => num(pair.second) === index)
      .map(pair => problem.tickets[num(pair.first)]?.id).filter(Boolean))] } : v,
    index, d: display?.[kind]?.[v.id] || {} }));
}
function columns(c, kind) {
  const p = c.draft.request.problem;
  const plain = (key, title, width = 105, parse = String, extra = {}) => ({ key, title, width, read: v => v[key] ?? '', write: (v, d, text) => {
    const value = parse(text); if (value === undefined) delete v[key]; else v[key] = value;
  }, ...extra });
  const time = (key, title, optional = false) => plain(key, title, 106, v => optional && !v.trim() ? undefined : parseTime(v), { read: v => v[key] === undefined && optional ? '' : stamp(v[key]) });
  const options = (key, title, choices, width = 145, extra = {}) => plain(key, title, width, String, {
    options: choices,
    read: v => String(v[key] ?? (choices.some(([id]) => id === '') ? '' : 0)),
    write(v, d, text) {
      const found = choices.find(([id, title]) => String(id) === text || title === text);
      if (!found) throw Error('请选择列表中的值');
      if (found[0] === '') delete v[key]; else v[key] = /^\d+$/.test(found[0]) ? Number(found[0]) : found[0];
    }, ...extra,
  });
  const location = (key, title) => options(key, title, list(p.locations).map((l, i) => [String(i), nameOf(c, 'locations', l.id)]), 158, { location: true });
  const skills = (key, title) => plain(key, title, 155, String, { tags: true,
    read: v => list(v[key]).join(', '), write: (v, d, text) => { v[key] = [...new Set(split(text))]; } });
  const dimensions = (key, title) => list(p.capacityDimensions).map((dim, i) => ({ key: `${key}.${i}`, title: `${dim.name} · ${title}`, width: 100,
    hint: dim.unit, read: v => String(v[key]?.[i] ?? 0), write(v, d, text) {
      const value = integer(text); v[key] = [...list(v[key])]; while (v[key].length < p.capacityDimensions.length) v[key].push('0'); v[key][i] = value;
    } }));
  if (kind === 'rules') return [plain('name', '约束', 230, String, { readonly: true }),
    ...[['hard', 'Hard'], ['medium', 'Medium'], ['soft', 'Soft']].map(([key, title]) => plain(key, title, 96, text => {
      const value = integer(text); if (+value > 1000000000) throw Error('权重范围为 0～1000000000'); return value;
    }, { readonly: v => v.id === 'executionConsistency' })),
    plain('setting', '设置', 130, String, { readonly: true, flex: true })];
  if (kind === 'costs') return [plain('name', '参数', 230, String, { readonly: true }),
    plain('value', '数值', 128, String, { write(v, d, text) {
      const value = integer(text);
      if (v.id !== 'restrictedChargePerHour' && +value > 10000) throw Error('排班余量范围为 0～10000‰');
      v.value = value;
    } }), plain('unit', '单位', 130, String, { readonly: true }),
    plain('note', '说明', 320, String, { readonly: true, flex: true })];
  const common = [plain('id', '编号', 100, String, { readonly: true }), { key: 'name', title: '名称', width: 142, read: (v, d) => d?.name || v.id,
    write(v, d, text) { if (!text.trim()) throw Error('名称不能为空'); d.name = text.trim(); } }];
  const tail = [{ key: 'note', title: '备注', width: 180, read: (v, d) => d?.note || '', write: (v, d, text) => { d.note = text; } }];
  if (kind === 'locations') return [...common,
    { key: 'address', title: '地址', width: 220, read: (v, d) => d?.address || '', write: (v, d, text) => { d.address = text; } },
    ...[['lng', '经度', 180], ['lat', '纬度', 90]].map(([key, title, limit]) => plain(key, title, 180, text => {
      const value = Number(text);
      if (!text.trim() || !Number.isFinite(value) || Math.abs(value) > limit) throw Error(`${title}范围为 -${limit}～${limit}`);
      return value;
    }, { read: v => String(v[key] ?? 0) })),
    plain('references', '关联对象', 145, String, { readonly: true, read: v => {
      const index = p.locations.findIndex(l => l.id === v.id);
      return `${list(p.tickets).filter(t => num(t.location) === index).length} 工单 · ${list(p.agents).filter(a => num(a.startLocation) === index || num(a.endLocation) === index).length} 执行单元`;
    } }), ...tail];
  if (kind === 'tickets') return [...common,
    options('type', '类型', types, 78, { read: v => typeof v.type === 'number' ? types[v.type][0] : v.type || types[0][0] }),
    location('location', '服务地点'),
    { key: 'predecessors', title: '前置工单', width: 200, predecessors: true,
      hint: '所有前置工单完成后才能开始，允许不同执行单元执行',
      read: v => list(v._predecessors).join(', '),
      write(v, d, text) {
        v._predecessors = [...new Set(String(text).split(/[,，、;；\n]/).map(s => s.trim()).filter(Boolean).map(token => {
          const exact = p.tickets.find(t => t.id === token);
          const found = exact ? [exact] : p.tickets.filter(t => nameOf(c, 'tickets', t.id) === token);
          if (found.length !== 1) throw Error(found.length ? `名称「${token}」不唯一，请使用编号` : `工单「${token}」不存在`);
          if (found[0].id === v.id) throw Error('工单不能依赖自身');
          return found[0].id;
        }))];
      } }, time('earliestStart', '最早开始'), time('latestStart', '最晚开始'),
    plain('duration', '时长 / 分', 85, String, { read: v => String(num(v.duration) / 60), write(v, d, text) {
      if (!text.trim() || !Number.isFinite(+text) || +text < 0 || !Number.isSafeInteger(Math.round(+text * 60))) throw Error('请输入有效的服务分钟数');
      v.duration = String(Math.round(+text * 60));
    } }), ...dimensions('demand', '需求'), skills('skillsRequired', '所需技能'), time('latestEnd', '最晚完成'),
    options('originalAgent', '原指派执行单元', [['', '未指定'], ...list(p.agents).map((a, i) => [String(i), nameOf(c, 'agents', a.id)])]),
    plain('pinned', '固定执行单元', 84, String, { options: [['false', '否'], ['true', '是']], read: v => String(!!v.pinned), write(v, d, text) {
      if (!['true', 'false', '是', '否'].includes(text)) throw Error('请输入是或否'); v.pinned = text === 'true' || text === '是';
    } }), plain('day', '服务日', 78, v => Number(integer(v)), { read: v => String(v.day || 0) }), plain('depotId', '网点'), ...tail];
  return [...common, location('startLocation', '出发地点'), location('endLocation', '返回地点'), time('startTime', '班次开始'), time('endTime', '班次结束', true),
    ...dimensions('capacity', '容量'), skills('skills', '执行单元技能'),
    plain('maxTickets', '接单上限', 90, v => Number(integer(v)), { read: v => String(v.maxTickets || 0), hint: '0 表示不限' }),
    plain('day', '服务日', 78, v => Number(integer(v)), { read: v => String(v.day || 0) }),
    plain('resourceId', '物理执行单元编号', 130, v => { if (!v.trim()) throw Error('物理执行单元编号不能为空'); return v.trim(); }, { readonly: !!c.op }),
    plain('depotId', '网点'), plain('fixedCost', '固定费用', 92, integer, { read: v => String(v.fixedCost || 0) }), ...tail];
}
function view(c, kind, options) {
  const cols = columns(c, kind), p = c.draft.request.problem, q = (c.sheetFilters?.[kind] || '').trim().toLowerCase();
  const originals = new Map(sourceRows(kind, c.scene.request.problem, c.scene.scenario.display, options).map(row => [row.v.id, row]));
  const rows = sourceRows(kind, p, c.draft.display, options).map(row => ({ ...row, original: originals.get(row.v.id) }))
    .filter(r => !q || cols.some(col => displayValue(col, r).toLowerCase().includes(q)));
  const sort = c.sheetSort?.[kind];
  if (sort) {
    const col = cols.find(col => col.key === sort.key);
    if (col) rows.sort((a, b) => displayValue(col, a).localeCompare(displayValue(col, b), 'zh-CN', { numeric: true }) * sort.direction || a.index - b.index);
  }
  return { cols, rows, kind, options, actions: !settings(kind), span: cols.length + (settings(kind) ? 1 : 2) };
}
function displayValue(col, row) {
  const value = String(col.read(row.v, row.d));
  return col.options?.find(([id]) => String(id) === value)?.[1] ?? value;
}
function cellKey(kind, index, key) { return `${kind}:${index}:${key}`; }
function rowsHtml(c, model, first) {
  const { cols, rows, kind } = model, last = Math.min(rows.length, first + WINDOW);
  let html = first ? `<tr class="sheet-spacer" aria-hidden="true"><td colspan="${model.span}" style="height:${first * ROW}px"></td></tr>` : '';
  for (let r = first; r < last; r++) {
    const row = rows[r], disabled = locked(c, kind, row.v);
    html += `<tr data-sheet-row="${r}"${disabled ? ' class="sheet-locked"' : ''}><th class="sheet-gutter"><button type="button" data-sheet-row-select="${r}" title="选择整行">${row.index + 1}</button></th>`;
    cols.forEach((col, j) => {
      const key = cellKey(kind, row.index, col.key), pending = c.sheetPending?.get(key), raw = String(col.read(row.v, row.d)), value = pending?.value ?? raw;
      const dirty = !row.original || String(col.read(row.original.v, row.original.d)) !== raw;
      const isReadonly = readonly(c, kind, row, col);
      const attrs = `id="sheet-${kind}-${row.index}-${esc(col.key)}" data-sheet-cell="${r},${j}" aria-label="${esc((row.v.name || row.v.id) + ' · ' + col.title)}" title="${esc(pending?.error || displayValue(col, row))}" ${pending ? 'aria-invalid="true"' : ''}${kind === 'rules' && ['hard', 'medium', 'soft'].includes(col.key) ? ` data-weight="${row.v.id}" data-level="${col.key}"` : ''}`;
      let control;
      if (col.options && !isReadonly) control = `<select ${attrs}>${col.options.map(([id, name]) => `<option value="${esc(id)}" ${String(id) === value ? 'selected' : ''}>${esc(name)}</option>`).join('')}${col.location ? '<option value="@new">＋ 新建地点…</option>' : ''}</select>`;
      else control = `<input type="text" ${attrs} value="${esc(isReadonly && col.options ? displayValue(col, row) : value)}" ${isReadonly ? 'readonly' : ''} ${col.tags ? 'list="sheet-skills"' : ''} autocomplete="off" spellcheck="false">`;
      html += `<td data-cell="${r},${j}" class="${j === 0 ? 'sheet-id ' : ''}${dirty ? 'sheet-dirty ' : ''}${pending ? 'sheet-invalid' : ''}"><div class="sheet-cell">${control}${col.predecessors ? `<button type="button" class="sheet-predecessors" data-sheet-predecessors="${row.index}" title="选择前置工单 · 全部完成后开始" aria-label="选择 ${esc(row.v.id)} 的前置工单" ${isReadonly ? 'disabled' : ''}>${list(row.v._predecessors).length > 2 ? '+' + (row.v._predecessors.length - 2) : '▾'}</button>` : ''}${col.location ? `<button type="button" class="sheet-location" data-action="linked-location" data-kind="${kind}" data-index="${row.index}" data-field="${col.key}" title="编辑关联地点" ${disabled ? 'disabled' : ''}>⌖</button>` : ''}</div></td>`;
    });
    if (model.actions) html += `<td class="sheet-row-actions"><button data-action="edit-object" data-kind="${kind}" data-index="${row.index}" title="完整字段" ${disabled ? 'disabled' : ''}>⋯</button><button data-action="delete-object" data-kind="${kind}" data-index="${row.index}" title="移除" ${c.op?.closed || c.op && kind !== 'locations' ? 'disabled' : ''}>×</button></td>`;
    html += '</tr>';
  }
  if (last < rows.length) html += `<tr class="sheet-spacer" aria-hidden="true"><td colspan="${model.span}" style="height:${(rows.length - last) * ROW}px"></td></tr>`;
  if (!rows.length) html += `<tr><td colspan="${model.span}">暂无匹配数据</td></tr>`;
  return html;
}
export function sheetPanel(c, kind, options = {}) {
  const model = view(c, kind, options), first = Math.max(0, Math.min(model.rows.length - WINDOW, Math.floor((c.sheetScroll?.[kind]?.y || 0) / ROW) - 12));
  c.sheetModels ||= {}; c.sheetModels[kind] = model;
  const skills = [...new Set([...list(c.draft.request.problem.tickets).flatMap(t => list(t.skillsRequired)), ...list(c.draft.request.problem.agents).flatMap(a => list(a.skills))])].sort();
  const help = kind === 'rules' ? '单元格直接编辑 · 支持 Excel 复制粘贴 · 权重按 Hard / Medium / Soft 依次比较，全 0 关闭 · 执行物理一致性只读'
    : kind === 'costs' ? '数值可直接编辑 · 余量仅影响排班时长，矩阵原值保留 · 修改先进入场景草稿'
    : kind === 'locations' ? '单元格直接编辑 · 支持 Excel 复制粘贴 · 移动共享地点会影响全部关联对象，仅相关行驶边改用估算'
    : '单元格直接编辑 · Tab / Enter 换格 · Shift 选择区域 · 支持 Excel 复制粘贴 · 橙色角标为修改';
  return `<section class="panel sheet-panel ${settings(kind) ? 'sheet-settings' : ''}" data-sheet="${kind}"><div class="data-toolbar sheet-toolbar">${settings(kind) ? `<strong>${titles[kind]}</strong>` : ''}<input id="${settings(kind) ? 'sheet-filter-' + kind : 'data-filter'}" data-sheet-filter="${kind}" class="main-search" placeholder="${settings(kind) ? '按名称筛选' : kind === 'locations' ? '搜索编号、名称、地址' : '搜索编号、名称、地点、技能'}" value="${esc(c.sheetFilters?.[kind] || '')}"><span>${model.rows.length} 条</span><button data-sheet-command="undo" ${c.sheetUndo?.length ? '' : 'disabled'} title="Ctrl+Z">撤销</button><button data-sheet-command="redo" ${c.sheetRedo?.length ? '' : 'disabled'} title="Ctrl+Y">重做</button><button data-sheet-command="fill" title="将选区首行填入后续行 · Ctrl+D">向下填充</button>${model.actions ? `<button data-action="edit-object" data-kind="${kind}">＋ 新增${titles[kind]}</button>` : ''}<span class="spacer"></span><button type="button" class="quiet sheet-help-button" data-action="sheet-help" data-help="${esc(help)}" aria-label="工作表操作帮助" title="${esc(help)}">?</button></div><div class="sheet-scroll" role="region" aria-label="${titles[kind]}工作表"><table class="data-table sheet-table" role="grid" aria-rowcount="${model.rows.length + 1}"><colgroup><col style="width:42px">${model.cols.map(col => col.flex ? '<col>' : `<col style="width:${col.width}px">`).join('')}${model.actions ? '<col style="width:66px">' : ''}</colgroup><thead><tr><th class="sheet-gutter">#</th>${model.cols.map((col, i) => `<th class="${i === 0 ? 'sheet-id' : ''}" title="${esc(col.hint || col.title)}"><button data-sheet-sort="${esc(col.key)}">${esc(col.title)}${c.sheetSort?.[kind]?.key === col.key ? c.sheetSort[kind].direction === 1 ? ' ↑' : ' ↓' : ''}</button></th>`).join('')}${model.actions ? '<th>操作</th>' : ''}</tr></thead><tbody>${rowsHtml(c, model, first)}</tbody></table></div><div class="sheet-status" role="status">${esc((!c.sheetErrorKind || c.sheetErrorKind === kind ? c.sheetError : '') || (c.dirty ? '草稿已修改，保存后用于重新规划。' : '已保存。行内修改先进入当前场景草稿。'))}</div>${!settings(kind) ? `<datalist id="sheet-skills">${skills.map(s => `<option value="${esc(s)}"></option>`).join('')}</datalist>` : ''}</section>`;
}
function bounds(c, model) {
  const selection = c.sheetSelection?.[model.kind] || { anchor: [0, 1], end: [0, 1] };
  return [Math.max(0, Math.min(selection.anchor[0], selection.end[0])), Math.min(model.rows.length - 1, Math.max(selection.anchor[0], selection.end[0])),
    Math.max(0, Math.min(selection.anchor[1], selection.end[1])), Math.min(model.cols.length - 1, Math.max(selection.anchor[1], selection.end[1]))];
}
function selectionClasses(root, c, model) {
  const [r0, r1, c0, c1] = bounds(c, model);
  root.querySelectorAll('[data-cell]').forEach(cell => { const [r, j] = cell.dataset.cell.split(',').map(Number); cell.classList.toggle('sheet-selected', r >= r0 && r <= r1 && j >= c0 && j <= c1); });
}
function validateRow(kind, v, count) {
  if (kind === 'tickets') {
    if (num(v.earliestStart) > num(v.latestStart)) throw Error('最早开始不能晚于最晚开始');
    if (num(v.latestEnd) < num(v.earliestStart)) throw Error('最晚完成不能早于最早开始');
    if (v.pinned && v.originalAgent === undefined) throw Error('固定工单需要原指派执行单元');
  } else if (kind === 'agents' && v.endTime !== undefined && num(v.endTime) < num(v.startTime)) throw Error('班次结束不能早于开始');
  for (const key of kind === 'tickets' ? ['location'] : kind === 'agents' ? ['startLocation', 'endLocation'] : [])
    if (num(v[key]) >= count) throw Error('关联地点不存在');
}
function applyCells(c, model, edits) {
  const staged = new Map();
  for (const [r, j, text] of edits) {
    const row = model.rows[r], col = model.cols[j];
    if (!row || !col) throw Error('粘贴范围超出当前工作表，请先新增所需行');
    if (readonly(c, model.kind, row, col)) {
      if (text !== displayValue(col, row) && text !== String(col.read(row.v, row.d))) throw Error(`第 ${r + 1} 行「${col.title}」只读`);
      continue;
    }
    if (!staged.has(row.index)) staged.set(row.index, { v: clone(row.v), d: clone(row.d), original: row });
    const next = staged.get(row.index);
    try { col.write(next.v, next.d, String(text).trim()); } catch (error) { throw Error(`第 ${r + 1} 行「${col.title}」：${error.message}`); }
  }
  for (const next of staged.values()) validateRow(model.kind, next.v, list(c.draft.request.problem.locations).length);
  for (const [index, next] of staged)
    if (JSON.stringify(next.v) === JSON.stringify(next.original.v) && JSON.stringify(next.d) === JSON.stringify(next.original.d)) staged.delete(index);
  const p = c.draft.request.problem;
  let precedences;
  if (model.kind === 'tickets' && [...staged.values()].some(next => JSON.stringify(next.v._predecessors) !== JSON.stringify(next.original.v._predecessors))) {
    const index = new Map(p.tickets.map((v, i) => [v.id, i]));
    precedences = list(p.precedences).filter(pair => !staged.has(num(pair.second)));
    for (const [second, next] of staged) for (const id of next.v._predecessors) precedences.push({ first: index.get(id), second });
    const edges = p.tickets.map(() => []), degree = p.tickets.map(() => 0);
    for (const {first, second} of precedences) {
      if (!p.tickets[first] || !p.tickets[second] || first === second) throw Error('前置工单引用无效');
      edges[first].push(second); degree[second]++;
    }
    const ready = degree.flatMap((n, i) => n ? [] : [i]);
    for (let i = 0; i < ready.length; i++) for (const next of edges[ready[i]]) if (--degree[next] === 0) ready.push(next);
    if (ready.length !== p.tickets.length) throw Error('前置工单形成循环依赖，请检查依赖链');
  }
  let matrices;
  if (model.kind === 'locations' && [...staged.values()].some(next => num(next.v.lng) !== num(next.original.v.lng) || num(next.v.lat) !== num(next.original.v.lat)))
    matrices = model.options.remapMatrices(p, p.locations.map((v, index) => staged.get(index)?.v || v));
  for (const [r, j] of edits) c.sheetPending?.delete(cellKey(model.kind, model.rows[r].index, model.cols[j].key));
  c.sheetError = ''; c.sheetErrorKind = '';
  if (!staged.size) return;
  checkpoint(c);
  if (!settings(model.kind)) c.draft.display[model.kind] ||= {};
  for (const [index, next] of staged) {
    if (model.kind === 'rules') {
      p.weights ||= {};
      safeSet(p.weights, next.v.id, { hard: next.v.hard, medium: next.v.medium, soft: next.v.soft });
    } else if (model.kind === 'costs') {
      if (next.v.id === 'restrictedChargePerHour') p.costs = { ...p.costs, restrictedChargePerHour: next.v.value };
      else {
        p.estimateBuffer ||= { modelPermille: 100, fallbackPermille: 200 };
        p.estimateBuffer[next.v.id] = Number(next.v.value);
      }
    } else {
      if (model.kind === 'tickets') { const { _predecessors, ...native } = next.v; p.tickets[index] = native; }
      else p[model.kind][index] = next.v;
      safeSet(c.draft.display[model.kind], next.v.id, next.d);
    }
    // A toolbar click can run before the deferred repaint (for example fill).
    next.original.v = next.v; next.original.d = next.d;
  }
  if (matrices) p.matrices = matrices;
  if (precedences) {
    const changedDependencies = JSON.stringify(precedences) !== JSON.stringify(list(p.precedences));
    p.precedences = precedences;
    const w = p.weights?.refTicketAfterDepTicket;
    if (changedDependencies && precedences.length && w && !num(w.hard) && !num(w.medium) && !num(w.soft))
      p.weights.refTicketAfterDepTicket = { hard: '1' };
  }
  changed(c);
}
export function parseTsv(text) {
  const rows = [[]]; let value = '', quoted = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === '"' && (quoted || value === '')) { if (quoted && text[i + 1] === '"') { value += '"'; i++; } else quoted = !quoted; }
    else if (!quoted && (ch === '\t' || ch === '\n')) { rows.at(-1).push(value.replace(/\r$/, '')); value = ''; if (ch === '\n') rows.push([]); }
    else value += ch;
  }
  rows.at(-1).push(value.replace(/\r$/, ''));
  if (rows.length > 1 && rows.at(-1).length === 1 && rows.at(-1)[0] === '') rows.pop();
  if (quoted) throw Error('粘贴内容的引号未闭合');
  return rows;
}
let dismissChoices = () => {};
function openPredecessors(c, model, index, render) {
  dismissChoices();
  const r = model.rows.findIndex(row => row.index === index), j = model.cols.findIndex(col => col.predecessors);
  if (r < 0 || j < 0) return;
  const row = model.rows[r], picked = new Set(row.v._predecessors), controls = new AbortController();
  const anchor = document.querySelector(`[data-sheet-predecessors="${index}"]`);
  const box = anchor.getBoundingClientRect(), popup = document.createElement('div');
  popup.className = 'sheet-choice'; popup.setAttribute('role', 'dialog'); popup.setAttribute('aria-label', '选择前置工单');
  popup.innerHTML = `<div class="sheet-choice-head"><strong>前置工单 · ${esc(row.v.id)}</strong><button type="button" data-choice-close aria-label="关闭">×</button></div><div class="sheet-choice-body"><input class="sheet-choice-search" aria-label="搜索前置工单" placeholder="搜索编号、名称" autocomplete="off"><p class="hint">全部前置完成后开始，可由不同执行单元执行。</p><div class="sheet-choice-list"></div><div class="sheet-choice-error" role="alert"></div></div><div class="sheet-choice-actions"><span class="muted"></span><button type="button" data-choice-close>取消</button><button type="button" class="primary" data-choice-apply>应用</button></div>`;
  popup.style.width = `${Math.min(360, innerWidth - 16)}px`;
  popup.style.left = `${Math.max(8, Math.min(box.right - 360, innerWidth - 368))}px`;
  document.body.append(popup);
  const search = popup.querySelector('.sheet-choice-search'), results = popup.querySelector('.sheet-choice-list');
  const choices = c.draft.request.problem.tickets.filter(t => t.id !== row.v.id);
  function draw() {
    const q = search.value.trim().toLowerCase();
    results.innerHTML = choices.filter(t => `${t.id} ${nameOf(c, 'tickets', t.id)}`.toLowerCase().includes(q)).map(t =>
      `<label><input type="checkbox" value="${esc(t.id)}" ${picked.has(t.id) ? 'checked' : ''}><span><b>${esc(t.id)}</b> ${esc(nameOf(c, 'tickets', t.id))}</span></label>`).join('') || '<p class="hint">没有匹配的工单</p>';
    popup.querySelector('.sheet-choice-actions span').textContent = `已选 ${picked.size} 项`;
  }
  dismissChoices = () => { controls.abort(); popup.remove(); dismissChoices = () => {}; };
  const options = { signal: controls.signal };
  popup.addEventListener('input', event => {
    if (event.target === search) draw();
    else if (event.target.type === 'checkbox') {
      if (event.target.checked) picked.add(event.target.value); else picked.delete(event.target.value);
      popup.querySelector('.sheet-choice-actions span').textContent = `已选 ${picked.size} 项`;
    }
  }, options);
  popup.addEventListener('click', event => {
    if (event.target.closest('[data-choice-close]')) { dismissChoices(); anchor.focus({ preventScroll: true }); }
    if (event.target.closest('[data-choice-apply]')) {
      try { applyCells(c, model, [[r, j, [...picked].join(', ')]]); dismissChoices(); render(); }
      catch (error) { popup.querySelector('.sheet-choice-error').textContent = error.message; }
    }
  }, options);
  document.addEventListener('pointerdown', event => { if (!popup.contains(event.target) && event.target !== anchor) dismissChoices(); }, { ...options, capture: true });
  popup.addEventListener('keydown', event => {
    if (event.key === 'Escape') { event.preventDefault(); dismissChoices(); anchor.focus({ preventScroll: true }); }
    if (event.key === 'Tab') {
      const nodes = [...popup.querySelectorAll('input,button')], first = nodes[0], last = nodes.at(-1);
      if (event.shiftKey && event.target === first) { event.preventDefault(); last.focus(); }
      if (!event.shiftKey && event.target === last) { event.preventDefault(); first.focus(); }
    }
  }, options);
  draw();
  popup.style.top = `${Math.max(8, Math.min(box.bottom + 4, innerHeight - popup.offsetHeight - 8))}px`;
  search.focus({ preventScroll: true });
}
export function mountSheet(c, { render, save, linkedLocation }) {
  dismissChoices();
  document.querySelectorAll('[data-sheet]').forEach(root => {
    const model = c.sheetModels?.[root.dataset.sheet];
    if (model) mountGrid(root, c, model, { render, save, linkedLocation });
  });
}
function mountGrid(root, c, model, { render, save, linkedLocation }) {
  const { kind } = model, scroll = root.querySelector('.sheet-scroll');
  let first = -1;
  c.sheetPending ||= new Map(); c.sheetSelection ||= {}; c.sheetScroll ||= {};
  const status = message => { c.sheetError = message; c.sheetErrorKind = kind; root.querySelector('.sheet-status').textContent = message; };
  const select = (r, j, extend = false) => {
    c.sheetSelection[kind] = { anchor: extend && c.sheetSelection[kind] ? c.sheetSelection[kind].anchor : [r, j], end: [r, j] };
    selectionClasses(root, c, model);
  };
  const focus = (r, j) => {
    r = Math.max(0, Math.min(model.rows.length - 1, r)); j = Math.max(0, Math.min(model.cols.length - 1, j));
    if (!model.rows[r]) return;
    c.sheetFocus = { kind, index: model.rows[r].index, key: model.cols[j].key };
    if (!root.querySelector(`[data-sheet-cell="${r},${j}"]`)) { scroll.scrollTop = r * ROW; drawRows(); }
    const el = root.querySelector(`[data-sheet-cell="${r},${j}"]`); el?.focus({ preventScroll: true });
    if (el) {
      const rect = el.getBoundingClientRect(), area = scroll.getBoundingClientRect();
      if (rect.top < area.top + ROW) scroll.scrollTop -= area.top + ROW - rect.top;
      if (rect.bottom > area.bottom - 18) scroll.scrollTop += rect.bottom - area.bottom + 18;
      const frozenWidth = 42 + (root.querySelector('.sheet-id')?.getBoundingClientRect().width || model.cols[0].width);
      if (j > 0 && rect.left < area.left + frozenWidth) scroll.scrollLeft -= area.left + frozenWidth - rect.left;
      if (rect.right > area.right - 18) scroll.scrollLeft += rect.right - area.right + 18;
    }
    if (el?.tagName === 'INPUT') el.select();
  };
  function drawRows() {
    const start = Math.max(0, Math.min(model.rows.length - WINDOW, Math.floor(scroll.scrollTop / ROW) - 12));
    if (first !== start) { first = start; root.querySelector('tbody').innerHTML = rowsHtml(c, model, first); selectionClasses(root, c, model); }
  }
  scroll.scrollTop = c.sheetScroll[kind]?.y || 0; scroll.scrollLeft = c.sheetScroll[kind]?.x || 0;
  first = Math.max(0, Math.min(model.rows.length - WINDOW, Math.floor(scroll.scrollTop / ROW) - 12));
  scroll.onscroll = () => { c.sheetScroll[kind] = { x: scroll.scrollLeft, y: scroll.scrollTop }; drawRows(); };
  selectionClasses(root, c, model);
  root.addEventListener('pointerdown', event => {
    const cell = event.target.closest('[data-cell]'); if (!cell) return;
    const [r, j] = cell.dataset.cell.split(',').map(Number); select(r, j, event.shiftKey);
    if (event.shiftKey) event.preventDefault();
  });
  root.addEventListener('focusin', event => {
    const el = event.target.closest('[data-sheet-cell]'); if (!el) return;
    const [r, j] = el.dataset.sheetCell.split(',').map(Number);
    c.sheetFocus = { kind, index: model.rows[r].index, key: model.cols[j].key };
    if (!c.sheetSelection[kind]) select(r, j);
  });
  root.addEventListener('change', event => {
    const el = event.target.closest('[data-sheet-cell]'); if (!el) return;
    const [r, j] = el.dataset.sheetCell.split(',').map(Number), col = model.cols[j], row = model.rows[r];
    if (col.location && el.value === '@new') { el.value = String(col.read(row.v, row.d)); linkedLocation(kind, row.index, col.key, true); return; }
    const key = cellKey(kind, row.index, col.key);
    try { applyCells(c, model, [[r, j, el.value]]); renderAfterChange(root, render); }
    catch (error) { c.sheetPending.set(key, { value: el.value, error: error.message }); el.setAttribute('aria-invalid', 'true'); el.closest('td').classList.add('sheet-invalid'); status(error.message); }
  });
  const fill = () => {
    const [r0, r1, j0, j1] = bounds(c, model); if (r0 === r1) throw Error('请先用 Shift 选择至少两行');
    const edits = []; for (let r = r0 + 1; r <= r1; r++) for (let j = j0; j <= j1; j++)
      if (!readonly(c, kind, model.rows[r], model.cols[j])) edits.push([r, j, String(model.cols[j].read(model.rows[r0].v, model.rows[r0].d))]);
    applyCells(c, model, edits); render();
  };
  root.addEventListener('click', event => {
    const button = event.target.closest('button'); if (!button) return;
    try {
      if (button.dataset.sheetPredecessors !== undefined) {
        const index = +button.dataset.sheetPredecessors;
        render();
        openPredecessors(c, c.sheetModels[kind], index, render);
        return;
      }
      if (button.dataset.sheetSort) { c.sheetSort ||= {}; const old = c.sheetSort[kind]; c.sheetSort[kind] = { key: button.dataset.sheetSort, direction: old?.key === button.dataset.sheetSort ? -old.direction : 1 }; c.sheetSelection[kind] = null; render(); }
      if (button.dataset.sheetRowSelect !== undefined) { const r = +button.dataset.sheetRowSelect; c.sheetSelection[kind] = { anchor: [r, 0], end: [r, model.cols.length - 1] }; selectionClasses(root, c, model); }
      if (button.dataset.sheetCommand === 'undo' || button.dataset.sheetCommand === 'redo') { undoSheet(c, button.dataset.sheetCommand === 'redo'); render(); }
      if (button.dataset.sheetCommand === 'fill') fill();
    } catch (error) { status(error.message); }
  });
  root.addEventListener('copy', event => {
    if (!event.target.closest('[data-sheet-cell]')) return;
    const [r0, r1, j0, j1] = bounds(c, model), rows = [];
    const quote = text => /[\t\n"]/.test(text) ? '"' + text.replaceAll('"', '""') + '"' : text;
    for (let r = r0; r <= r1; r++) rows.push(model.cols.slice(j0, j1 + 1).map(col => quote(displayValue(col, model.rows[r]))).join('\t'));
    event.clipboardData.setData('text/plain', rows.join('\r\n')); event.preventDefault();
  });
  root.addEventListener('paste', event => {
    const el = event.target.closest('[data-sheet-cell]'); if (!el) return;
    event.preventDefault();
    try {
      const [r, j] = el.dataset.sheetCell.split(',').map(Number), pasted = parseTsv(event.clipboardData.getData('text/plain'));
      applyCells(c, model, pasted.flatMap((row, i) => row.map((text, k) => [r + i, j + k, text]))); render();
    } catch (error) { status('粘贴未应用：' + error.message); }
  });
  root.addEventListener('keydown', event => {
    const el = event.target.closest('[data-sheet-cell]'); if (!el) return;
    const [r, j] = el.dataset.sheetCell.split(',').map(Number), command = event.ctrlKey || event.metaKey;
    try {
      if (command && ['z', 'y', 'd', 's'].includes(event.key.toLowerCase())) {
        event.preventDefault();
        const key = event.key.toLowerCase();
        if (key === 'z' || key === 'y') { undoSheet(c, key === 'y' || event.shiftKey); render(); }
        else if (key === 'd') fill(); else { el.blur(); save(); }
        return;
      }
      if (event.key === 'Escape') { c.sheetPending.delete(cellKey(kind, model.rows[r].index, model.cols[j].key)); c.sheetError = ''; render(); return; }
      if (event.key === 'F2') { event.preventDefault(); el.dataset.textEditing = 'true'; return; }
      let dr = 0, dj = 0;
      if (event.key === 'Enter') dr = event.shiftKey ? -1 : 1;
      else if (event.key === 'Tab') dj = event.shiftKey ? -1 : 1;
      else if (!el.dataset.textEditing && event.key.startsWith('Arrow') && (el.tagName !== 'SELECT' || event.ctrlKey)) {
        dr = event.key === 'ArrowDown' ? 1 : event.key === 'ArrowUp' ? -1 : 0;
        dj = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0;
      } else return;
      event.preventDefault();
      let nr = r + dr, nj = j + dj;
      if (nj >= model.cols.length) { nj = 1; nr++; } else if (nj < 0) { nj = model.cols.length - 1; nr--; }
      const extend = event.shiftKey && event.key.startsWith('Arrow');
      el.blur();
      // blur may replace the grid after committing a cell; defer to the next mount.
      const row = model.rows[Math.max(0, Math.min(model.rows.length - 1, nr))];
      if (row) { c.sheetFocus = { kind, index: row.index, key: model.cols[nj].key }; select(Math.max(0, Math.min(model.rows.length - 1, nr)), nj, extend); render(); }
    } catch (error) { status(error.message); }
  });
  const active = document.activeElement;
  if (c.sheetFocus?.kind === kind && (active === document.body || active?.matches('[data-sheet-cell]'))) {
    const r = model.rows.findIndex(row => row.index === c.sheetFocus.index), j = model.cols.findIndex(col => col.key === c.sheetFocus.key);
    if (r >= 0 && j >= 0 && r >= first && r < first + WINDOW) focus(r, j);
  }
}
