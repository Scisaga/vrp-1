import {
  list,
  num,
  esc,
  clone,
  stamp,
  parseTime,
  label,
  icon,
} from "./schedule.js";

import { sheetPanel } from "./sheet.js";
import { matrixBytes, matrixPanel } from "./matrix.js";

export const field = (title, name, value = "", type = "text", attrs = "") =>
  `<label>${esc(title)}<input name="${esc(name)}" type="${type}" value="${esc(value)}" ${attrs}></label>`;
export const select = (title, name, items, value = "") =>
  `<label>${esc(title)}<select name="${esc(name)}">${items.map(([v, t]) => `<option value="${esc(v)}" ${String(v) === String(value) ? "selected" : ""}>${esc(t)}</option>`).join("")}</select></label>`;
export const check = (title, name, on = false) =>
  `<label class="check"><input type="checkbox" name="${esc(name)}" ${on ? "checked" : ""}>${esc(title)}</label>`;
export const grid = (html) => `<div class="form-grid">${html}</div>`;
export const csv = (v) =>
  String(v || "")
    .split(/[,，\n]/)
    .map((v) => v.trim())
    .filter(Boolean);
export const defaults = {
  agentCapacity: { hard: "1" },
  agentMaxTicket: { hard: "1" },
  agentSkillsAccordWithTicketSkills: { hard: "1" },
  agentQualificationLevelsMatchTicket: { medium: "100" },
  refTicketAfterDepTicket: { hard: "1" },
  ticketStartServiceTimeMatchExpected: { soft: "100" },
  ticketArrivalTimeSameDateWithPlanTime: { hard: "1" },
  relationTicketsSameAgent: { medium: "50" },
  minimizeTravelTime: { soft: "1" },
  minimizeAgentFixedCost: { soft: "20" },
  sameDepo: { hard: "1" },
  balanceAgentLoading: { medium: "1" },
  minimizeTicketChanging: { soft: "1000" },
  ticketUnassigned: { medium: "1000" },
  unreachableLeg: { hard: "1" },
  agentShiftOvertime: { soft: "100" },
  executionConsistency: { hard: "1" },
  minimizeVisitOrderChanging: { soft: "10" },
  minimizeServiceTimeChanging: { soft: "1" },
};
export const locations = (c) =>
  list(c.draft.request.problem.locations).map((l, i) => [
    String(i),
    label(c.draft.display, "locations", l.id),
  ]);
export const agents = (c) =>
  list(c.draft.request.problem.agents).map((a, i) => [
    String(i),
    label(c.draft.display, "agents", a.id),
  ]);
export function beginDraft(c) {
  if (c.draft) return;
  c.draft = {
    request: clone(c.scene.request),
    display: clone(c.scene.scenario.display || {}),
    name: c.scene.scenario.name,
    origin: clone(c.scene.scenario.origin || {}),
  };
  c.draftVersion = c.scene.scenario.version;
  c.dirty = false;
}
function encodeBytes(bytes) {
  let value = "";
  for (let offset = 0; offset < bytes.length; offset += 32768)
    value += String.fromCharCode(...bytes.subarray(offset, offset + 32768));
  return btoa(value);
}
// Preserve unchanged directed edges when editing locations. Only incident edges
// of new/moved locations become UNKNOWN and use the shared engine estimator.
function remapMatrices(problem, nextLocations) {
  const previous = new Map(
    problem.locations.map((location, index) => [
      location.id,
      { location, index },
    ]),
  );
  const indices = nextLocations.map((location) => {
    const old = previous.get(location.id);
    return old &&
      num(old.location.lng) === num(location.lng) &&
      num(old.location.lat) === num(location.lat)
      ? old.index
      : -1;
  });
  const oldN = problem.locations.length,
    n = nextLocations.length;
  return list(problem.matrices).map((matrix) => {
    const old = matrixBytes(matrix);
    const durations = new Uint8Array(n * n * 4),
      distances = new Uint8Array(n * n * 4),
      sources = new Uint8Array(n * n);
    const ds = new DataView(durations.buffer),
      ms = new DataView(distances.buffer);
    for (let from = 0; from < n; from++)
      for (let to = 0; to < n; to++) {
        const k = from * n + to,
          previous = indices[from] * oldN + indices[to];
        if (from === to) continue;
        const keep =
          indices[from] >= 0 &&
          indices[to] >= 0 &&
          (previous + 1) * 4 <= old.durations.byteLength &&
          (previous + 1) * 4 <= old.distances.byteLength;
        ds.setUint32(
          k * 4,
          keep ? old.durations.getUint32(previous * 4, true) : 4294967294,
          true,
        );
        ms.setUint32(
          k * 4,
          keep ? old.distances.getUint32(previous * 4, true) : 4294967294,
          true,
        );
        sources[k] = keep ? old.sources[previous] || 0 : 3;
      }
    return {
      ...matrix,
      durations: encodeBytes(durations),
      distances: encodeBytes(distances),
      sources: encodeBytes(sources),
    };
  });
}
export function dataPanel(c) {
  beginDraft(c);
  const p = c.draft.request.problem,
    tab = c.dataTab === "relations" ? "tickets" : c.dataTab || "tickets";
  c.sheetModels = {};
  const tabs = [
    ["tickets", "工单", list(p.tickets).length],
    ["agents", "执行单元", list(p.agents).length],
    ["locations", "地点", list(p.locations).length],
    ["matrix", "在途矩阵", null],
    ["rules", "约束与费用", null],
    ["advanced", "原始输入", null],
  ];
  const date = p.horizon?.startLocal?.split("T")[0] || "设置业务日期";
  const tabIcons = { tickets: "clipboard", agents: "team", locations: "pin", matrix: "matrix", rules: "scale", advanced: "code" };
  const html = `<div class="data-nav"><nav class="data-tabs tab-strip" role="tablist" aria-label="场景数据">${tabs.map(([id, name, count]) => `<button type="button" id="data-tab-${id}" data-data-tab="${id}" role="tab" aria-selected="${tab === id}" aria-controls="data-tab-panel" tabindex="${tab === id ? 0 : -1}" class="${tab === id ? "selected" : ""}">${icon(tabIcons[id])}<span>${name}</span>${count !== null ? ` <span class="data-tab-count">${count}</span>` : ""}</button>`).join("")}</nav><div class="data-tools"><button type="button" class="quiet scene-date" data-action="scene-date" aria-label="编辑业务日期" title="${c.op ? "已有执行，业务日期不可更改" : "点击编辑业务日期"}" ${c.op ? "disabled" : ""}>${esc(date)} <span aria-hidden="true">▾</span></button><button type="button" class="quiet scene-settings" data-action="scene-properties" aria-label="场景设置" title="场景设置：名称、日期、时区和容量单位">⚙</button></div></div>`;
  const panel = (content) => `${html}<div id="data-tab-panel" class="data-tab-panel" role="tabpanel" aria-labelledby="data-tab-${tab}">${content}</div>`;
  if (tab === "matrix") return panel(matrixPanel(c));
  if (["tickets", "agents", "locations"].includes(tab)) return panel(sheetPanel(c, tab, { remapMatrices }));
  if (tab === "advanced")
    return panel(
      `<section class="panel"><div class="panel-head"><div><h2>原生求解输入</h2><p class="hint">用于高级字段、矩阵与批量修改。应用到草稿后，保存时进行完整校验。</p></div><button data-action="apply-json">应用到草稿</button></div><textarea id="json-editor" class="json-editor" spellcheck="false" aria-label="原生求解输入">${esc(c.rawDraft ?? JSON.stringify(c.draft.request, null, 2))}</textarea></section>`
    );
  if (tab === "rules") return panel(sheetPanel(c, "rules", { defaults }) + sheetPanel(c, "costs"));
  return panel("");
}

export function objectEditor(c, kind, index) {
  beginDraft(c);
  const p = c.draft.request.problem,
    d = c.draft.display,
    old = list(p[kind])[index],
    value = clone(old || {}),
    display = d[kind]?.[value.id] || {},
    existing = !!old;
  let html =
    field(
      "编号",
      "id",
      value.id ||
        `${{ tickets: "T", agents: "V", locations: "L" }[kind]}-${Date.now().toString(36)}`,
      "text",
      `required ${existing ? "readonly" : ""}`,
    ) +
    field("名称", "name", kind === "agents" ? label(d, kind, value.id) : display.name || value.id || "", "text", "required");
  if (kind === "tickets") {
    html +=
      select("服务地点", "location", locations(c), value.location || 0) +
      select(
        "工单类型",
        "type",
        [
          [0, "上门服务"],
          [1, "配送"],
          [2, "取件"],
        ],
        typeof value.type === "number"
          ? value.type
          : [
              "TICKET_TYPE_SERVICE",
              "TICKET_TYPE_DELIVERY",
              "TICKET_TYPE_PICKUP",
            ].indexOf(value.type || "TICKET_TYPE_SERVICE"),
      ) +
      field(
        "服务时长（分钟）",
        "duration",
        existing ? num(value.duration) / 60 : 20,
        "number",
        'min="0" step="0.1" required',
      ) +
      field(
        "服务日（0 为首日）",
        "day",
        value.day || 0,
        "number",
        'min="0" required',
      ) +
      field(
        "最早开始",
        "earliestStart",
        stamp(value.earliestStart ?? 28800),
        "text",
        "required",
      ) +
      field(
        "最晚开始",
        "latestStart",
        stamp(value.latestStart ?? 61200),
        "text",
        "required",
      ) +
      field(
        "最晚完成",
        "latestEnd",
        stamp(value.latestEnd ?? 64800),
        "text",
        "required",
      ) +
      field(
        "需求量（逗号分隔）",
        "demand",
        list(value.demand).join(",") ||
          list(p.capacityDimensions)
            .map(() => 0)
            .join(","),
      ) +
      field(
        "所需技能（逗号分隔）",
        "skillsRequired",
        list(value.skillsRequired).join(","),
      ) +
      field("所属网点编号（可空）", "depotId", value.depotId) +
      select(
        "原指派执行单元",
        "originalAgent",
        [["", "未指定"], ...agents(c)],
        value.originalAgent ?? "",
      ) +
      check("固定到原指派执行单元", "pinned", value.pinned) +
      `<p class="hint">容量维度顺序：${
        list(p.capacityDimensions)
          .map((v) => esc(v.name + " / " + v.unit))
          .join("，") || "无"
      }。时间可用 08:30，跨天用 1d 08:30。</p>`;
  } else if (kind === "agents") {
    html +=
      field(
        "物理执行单元编号",
        "resourceId",
        value.resourceId ||
          `${c.id}-${value.id || "resource-" + Date.now().toString(36)}`,
        "text",
        "required",
      ) +
      field(
        "服务日（0 为首日）",
        "day",
        value.day || 0,
        "number",
        'min="0" required',
      ) +
      select(
        "出发地点",
        "startLocation",
        locations(c),
        value.startLocation || 0,
      ) +
      select("返回地点", "endLocation", locations(c), value.endLocation || 0) +
      field(
        "班次开始",
        "startTime",
        stamp(value.startTime ?? 28800),
        "text",
        "required",
      ) +
      field(
        "班次结束（可空）",
        "endTime",
        value.endTime === undefined
          ? existing
            ? ""
            : "18:00"
          : stamp(value.endTime),
      ) +
      field(
        "容量（逗号分隔）",
        "capacity",
        list(value.capacity).join(",") ||
          list(p.capacityDimensions)
            .map(() => 20)
            .join(","),
      ) +
      field(
        "最大接单数（0 不限）",
        "maxTickets",
        value.maxTickets || 0,
        "number",
        'min="0" required',
      ) +
      field("技能（逗号分隔）", "skills", list(value.skills).join(",")) +
      field("所属网点编号（可空）", "depotId", value.depotId) +
      field(
        "固定费用（整数单位）",
        "fixedCost",
        value.fixedCost || 0,
        "number",
        'min="0" required',
      ) +
      field(
        "行驶矩阵序号",
        "matrix",
        value.matrix || 0,
        "number",
        'min="0" required',
      ) +
      `<p class="hint">物理执行单元编号用于防止同一车辆或人员在多个执行场景中被重复占用。容量顺序：${
        list(p.capacityDimensions)
          .map((v) => esc(v.name))
          .join("，") || "无"
      }。</p>`;
  } else
    html +=
      field("地址", "address", display.address) +
      field(
        "经度",
        "lng",
        value.lng ?? 121.47,
        "number",
        'min="-180" max="180" step="any" required',
      ) +
      field(
        "纬度",
        "lat",
        value.lat ?? 31.23,
        "number",
        'min="-90" max="90" step="any" required',
      ) +
      '<p class="hint">新增或移动地点时，仅涉及该地点的行驶边改用统一几何估算，其余矩阵值保留。估算不代表道路路径。</p>';
  html += field("业务备注", "note", display.note);
  return {
    html: grid(html),
    apply(f) {
      const id = String(f.get("id")).trim();
      if (!id) throw Error("编号不能为空");
      if (!existing && list(p[kind]).some((v) => v.id === id))
        throw Error("编号已存在");
      value.id = id;
      if (kind === "tickets") {
        for (const key of ["location", "day", "type"])
          value[key] = Number(f.get(key));
        for (const key of ["earliestStart", "latestStart", "latestEnd"])
          value[key] = parseTime(f.get(key));
        value.duration = String(Math.round(Number(f.get("duration")) * 60));
        value.demand = csv(f.get("demand"));
        value.skillsRequired = csv(f.get("skillsRequired"));
        value.depotId = f.get("depotId").trim();
        value.pinned = f.has("pinned");
        if (f.get("originalAgent") === "") delete value.originalAgent;
        else value.originalAgent = Number(f.get("originalAgent"));
        if (value.pinned && value.originalAgent === undefined)
          throw Error("固定工单需要选择原指派执行单元");
      } else if (kind === "agents") {
        for (const key of [
          "startLocation",
          "endLocation",
          "day",
          "matrix",
          "maxTickets",
        ])
          value[key] = Number(f.get(key));
        value.startTime = parseTime(f.get("startTime"));
        if (f.get("endTime").trim())
          value.endTime = parseTime(f.get("endTime"));
        else delete value.endTime;
        value.capacity = csv(f.get("capacity"));
        value.skills = csv(f.get("skills"));
        for (const key of ["resourceId", "depotId", "fixedCost"])
          value[key] = f.get(key).trim();
      } else {
        const changed =
          !old ||
          num(old.lng) !== Number(f.get("lng")) ||
          num(old.lat) !== Number(f.get("lat"));
        value.lng = Number(f.get("lng"));
        value.lat = Number(f.get("lat"));
        if (changed) {
          const next = [...list(p.locations)];
          if (existing) next[index] = value;
          else next.push(value);
          p.matrices = remapMatrices(p, next);
        }
      }
      p[kind] ||= [];
      if (existing) p[kind][index] = value;
      else p[kind].push(value);
      d[kind] ||= {};
      Object.defineProperty(d[kind], id, {
        enumerable: true,
        configurable: true,
        writable: true,
        value: {
          name: f.get("name").trim(),
          note: f.get("note").trim(),
          ...(kind === "locations"
            ? { address: f.get("address").trim() }
            : { address: display.address || "" }),
        },
      });
      c.dirty = true;
      c.rawDirty = false;
      delete c.rawDraft;
    },
  };
}
export function removeObject(c, kind, index) {
  const p = c.draft.request.problem,
    v = p[kind][index];
  if (!v) throw Error("对象不存在");
  if (c.op && kind !== "locations")
    throw Error(
      "执行中的工单请通过取消事件处理；执行单元请设置为不可用，保留执行历史",
    );
  if (kind === "locations") {
    if (
      list(p.tickets).some((t) => (t.location || 0) === index) ||
      list(p.agents).some(
        (a) =>
          (a.startLocation || 0) === index || (a.endLocation || 0) === index,
      )
    )
      throw Error("地点仍被工单或执行单元引用，请先调整引用");
    p.matrices = remapMatrices(
      p,
      p.locations.filter((_, i) => i !== index),
    );
    for (const t of list(p.tickets)) if (num(t.location) > index) t.location--;
    for (const a of list(p.agents))
      for (const k of ["startLocation", "endLocation"])
        if (num(a[k]) > index) a[k]--;
  } else if (kind === "agents") {
    if (list(p.tickets).some((t) => t.originalAgent === index))
      throw Error("执行单元仍被原指派引用，请先调整工单");
    for (const t of list(p.tickets))
      if (t.originalAgent > index) t.originalAgent--;
    delete c.draft.request.initialSolution;
  } else {
    for (const k of ["relatedPairs", "precedences"]) {
      if (
        list(p[k]).some(
          (r) => (r.first || 0) === index || (r.second || 0) === index,
        )
      )
        throw Error("工单仍被关联或前置关系引用，请先移除关系");
    }
    for (const k of ["relatedPairs", "precedences"])
      for (const r of list(p[k]))
        for (const side of ["first", "second"])
          if (num(r[side]) > index) r[side]--;
    delete c.draft.request.initialSolution;
  }
  p[kind].splice(index, 1);
  delete c.draft.display[kind]?.[v.id];
  c.dirty = true;
  delete c.rawDraft;
}
export const solveFields = (options = {}, max = 1) =>
  grid(
    select(
      "求解档位",
      "preset",
      [
        [0, "快速 · 总时限 30 秒"],
        [1, "标准 · 总时限 120 秒"],
        [2, "深度 · 300 秒无改进停止"],
      ],
      typeof options.preset === "number"
        ? options.preset
        : [
            "SOLVE_PRESET_FAST",
            "SOLVE_PRESET_STANDARD",
            "SOLVE_PRESET_DEEP",
          ].indexOf(options.preset || "SOLVE_PRESET_FAST"),
    ) +
      field(
        "单任务线程数",
        "threads",
        options.threads || Math.min(2, max),
        "number",
        `min="1" max="${max}" required`,
      ),
  ) +
  `<details class="advanced"><summary>高级求解参数</summary>${grid(
    select(
      "搜索策略",
      "searchStrategy",
      [
        [0, "邻域搜索"],
        [1, "基准搜索"],
        [2, "α-nearness"],
        [3, "α-nearness + k-opt"],
      ],
      typeof options.searchStrategy === "number"
        ? options.searchStrategy
        : [
            "SEARCH_STRATEGY_NEAREST",
            "SEARCH_STRATEGY_BASELINE",
            "SEARCH_STRATEGY_ALPHA",
            "SEARCH_STRATEGY_ALPHA_K_OPT",
          ].indexOf(options.searchStrategy || "SEARCH_STRATEGY_ALPHA_K_OPT"),
    ) +
      field(
        "总时限覆盖（秒，可空）",
        "timeLimit",
        num(options.timeLimitMs) ? num(options.timeLimitMs) / 1000 : "",
        "number",
        'min="0.001" step="0.001"',
      ) +
      field(
        "无改进时限覆盖（秒，可空）",
        "unimproved",
        num(options.unimprovedStopMs)
          ? num(options.unimprovedStopMs) / 1000
          : "",
        "number",
        'min="0.001" step="0.001"',
      ) +
      field(
        "随机种子",
        "seed",
        options.seed || 0,
        "number",
        'min="0" required',
      ),
  )}</details><p class="hint">排队时间不占求解时限。深度档每次找到更优解后重新计算无改进时间，也可设置总时限。</p>`;
export function readOptions(f) {
  return {
    preset: Number(f.get("preset")),
    threads: Number(f.get("threads")),
    searchStrategy: Number(f.get("searchStrategy")),
    seed: f.get("seed") || "0",
    timeLimitMs: f.get("timeLimit")
      ? String(Math.round(Number(f.get("timeLimit")) * 1000))
      : "0",
    unimprovedStopMs: f.get("unimproved")
      ? String(Math.round(Number(f.get("unimproved")) * 1000))
      : "0",
  };
}
export function freshProblem(id) {
  return {
    problem: {
      id,
      horizon: {
        startLocal: new Date().toLocaleDateString("sv-SE") + "T00:00:00",
        timezone: "Asia/Shanghai",
      },
      locations: [{ id: "DEPOT", lng: 121.47, lat: 31.23 }],
      capacityDimensions: [{ name: "件数", unit: "件", penaltyUnit: "1" }],
      agents: [],
      tickets: [],
    },
    options: { preset: 0, threads: 1, searchStrategy: 3 },
  };
}
