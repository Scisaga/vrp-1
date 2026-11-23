export const list = (v) => v || [];
export const num = (v) => Number(v || 0);
export const esc = (v) =>
  String(v ?? "").replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        c
      ],
  );
export const clone = (v) => structuredClone(v);
export const icon = (name) => `<svg class="ui-icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false"><use href="#icon-${esc(name)}"/></svg>`;
export function stamp(value) {
  const s = Math.max(0, Math.floor(num(value))),
    d = Math.floor(s / 86400);
  return `${d ? d + "d " : ""}${String(Math.floor(s / 3600) % 24).padStart(2, "0")}:${String(Math.floor(s / 60) % 60).padStart(2, "0")}${s % 60 ? ":" + String(s % 60).padStart(2, "0") : ""}`;
}
export function parseTime(value) {
  const s = String(value).trim();
  if (/^\d+$/.test(s)) return s;
  const m = /^(?:(\d+)d\s*)?(\d{1,2}):(\d{2})(?::(\d{2}))?$/.exec(s);
  if (!m || +m[2] > 23 || +m[3] > 59 || +(m[4] || 0) > 59)
    throw Error("时间格式应为 08:30，跨天可填 1d 08:30");
  return String(
    +(m[1] || 0) * 86400 + +m[2] * 3600 + +m[3] * 60 + +(m[4] || 0),
  );
}
export const phaseNames = [
  "待执行",
  "前往中",
  "已到达",
  "服务中",
  "已完成",
  "已取消",
  "异常",
];
export const vehicleNames = [
  "可调度",
  "行驶中",
  "等待中",
  "服务中",
  "暂停",
  "不可用",
];
export const jobNames = [
  "排队中",
  "求解中",
  "正在停止",
  "已完成",
  "已停止",
  "失败",
  "重启中断",
];
export const kindNames = [
  "规划求解",
  "继续优化",
  "重新规划",
  "实时重排",
  "人工调整",
];
const enumValue = (value, prefix, names) =>
  typeof value === "number"
    ? value
    : Math.max(0, names.indexOf(String(value || "").replace(prefix, "")));
export const tp = (t) =>
  enumValue(t?.phase, "TICKET_PHASE_", [
    "PENDING",
    "EN_ROUTE",
    "ARRIVED",
    "SERVING",
    "COMPLETED",
    "CANCELLED",
    "EXCEPTION",
  ]);
export const vp = (v) =>
  enumValue(v?.phase, "VEHICLE_PHASE_", [
    "AVAILABLE",
    "TRAVELLING",
    "WAITING",
    "SERVICING",
    "PAUSED",
    "UNAVAILABLE",
  ]);
export const jp = (j) =>
  enumValue(j?.state, "JOB_STATE_", [
    "QUEUED",
    "RUNNING",
    "CANCELLING",
    "SUCCEEDED",
    "CANCELLED",
    "FAILED",
    "INTERRUPTED",
  ]);
export const jk = (j) =>
  j?.kind === undefined && j?.executionBasis
    ? 3
    : enumValue(j?.kind, "JOB_KIND_", [
        "PLANNING",
        "OPTIMIZATION",
        "REPLANNING",
        "REALTIME_REPLAN",
        "MANUAL_ADJUSTMENT",
      ]);
export const label = (display, group, id) => {
  if (id?.startsWith("@vrp/load/")) return "装货 · " + label(display, "tickets", id.slice(10));
  if (id?.startsWith("@vrp/unload/")) return "卸货 · " + label(display, "tickets", id.slice(12));
  const name = Object.hasOwn(display?.[group] || {}, id) ? display[group][id]?.name || id : id;
  // Translate generic labels from the existing demo without rewriting stable
  // IDs or customer-provided names in saved inputs and execution history.
  return group === "agents" ? String(name ?? "").replace(/^资源(?=\d+$)/, "执行单元") : name;
};
export const scoreNumber = (value) => BigInt(value ?? 0).toLocaleString("en-US");
export const score = (s) =>
  `<span class="score"><span class="hard">${scoreNumber(s?.hard)} Hard</span> / <span class="medium">${scoreNumber(s?.medium)} Medium</span> / <span class="soft">${scoreNumber(s?.soft)} Soft</span></span>`;
export const kv = (rows) =>
  `<dl class="kv">${rows.map(([key, value]) => `<dt>${esc(key)}</dt><dd>${value}</dd>`).join("")}</dl>`;
export const badge = (text, tone = "") =>
  `<span class="badge ${tone}">${esc(text)}</span>`;
export const empty = (title, text, action = "") =>
  `<div class="empty-state"><span class="empty-icon">▤</span><h2>${esc(title)}</h2><p>${esc(text)}</p>${action}</div>`;
export const constraintNames = {
  agentCapacity: "容量上限",
  agentMaxTicket: "接单数上限",
  agentSkillsAccordWithTicketSkills: "技能匹配",
  agentQualificationLevelsMatchTicket: "资质等级",
  refTicketAfterDepTicket: "前置工单完成后服务",
  refTicketSameAgentWithDepTicket: "前后置工单同执行单元",
  serviceFinishedAfterMaxEndTime: "最晚完成时间",
  ticketStartServiceTimeMatchExpected: "预约开始时间",
  ticketArrivalTimeSameDateWithPlanTime: "计划服务日",
  relationTicketsSameAgent: "关联工单同执行单元",
  minimizeTravelTime: "行驶时间",
  minimizeTravelDistance: "行驶距离",
  minimizeAgentFixedCost: "执行单元固定费用",
  sameDepo: "所属网点",
  balanceAgentLoading: "接单量均衡",
  balanceAgentLoadingRatio: "载荷比例均衡",
  balanceAgentWorkingTime: "工作时长均衡",
  minimizeTicketChanging: "减少改派",
  ticketUnassigned: "未分配工单",
  unreachableLeg: "路径不可达",
  agentShiftOvertime: "班次超时",
  executionConsistency: "执行物理一致性",
  minimizeVisitOrderChanging: "减少顺序变化",
  minimizeServiceTimeChanging: "减少预约变化",
};
export const constraintTitle = (key) =>
  constraintNames[key.replace(/_([a-z])/g, (_, c) => c.toUpperCase())] || key;
export const reasonTitle = (key) =>
  ({
    not_assigned: "本次未找到安排",
    no_eligible_agent: "没有符合结构限制的执行单元",
    execution_blocked: "执行状态禁止安排",
    cargo_owner_unavailable: "货物所属执行单元不可用",
    no_available_vehicle: "当前无可用执行单元",
    skills: "技能要求不匹配",
    qualification: "资质不匹配",
    depot: "网点不匹配",
    day: "服务日期不匹配",
    pinned: "固定归属限制",
  })[key] || key;
export function resultRoutes(input, result) {
  const p = input?.problem || {};
  return list(result?.routes).map((r) => {
    const a = list(p.agents)[r.agent || 0];
    return {
      agentId: a?.id,
      startTime: a?.startTime,
      endTime: r.endTime,
      travelTime: r.travelTime,
      travelDistance: r.travelDistance,
      stops: list(r.stops).map((s) => {
        const t = list(p.tickets)[s.ticket || 0];
        return {
          ...s,
          ticketId: t?.id,
          locationId: list(p.locations)[t?.location || 0]?.id,
          action: t?.id?.startsWith("@vrp/load/")
            ? "load"
            : t?.id?.startsWith("@vrp/unload/")
              ? "unload"
              : "service",
        };
      }),
    };
  });
}
export function planRoutes(input, plan) {
  return list(plan?.routes).map((r) => ({
    ...r,
    startTime:
      r.startTime ??
      list(input?.problem?.agents).find((a) => a.id === r.agentId)?.startTime,
  }));
}
export function assignments(routes) {
  return new Map(
    list(routes).flatMap((r) =>
      list(r.stops).map((s, i) => [
        s.ticketId,
        { ...s, agentId: r.agentId, order: i },
      ]),
    ),
  );
}
export function changes(before, after) {
  const a = assignments(before),
    b = assignments(after),
    ids = new Set([...a.keys(), ...b.keys()]);
  return [...ids]
    .filter((id) => !id.startsWith("@vrp/"))
    .flatMap((id) => {
      const x = a.get(id),
        y = b.get(id);
      if (
        !x ||
        !y ||
        x.agentId !== y.agentId ||
        x.order !== y.order ||
        num(x.start) !== num(y.start)
      )
        return [{ id, before: x, after: y }];
      return [];
    });
}
export function nowAt(op) {
  return op
    ? num(op.at) +
        (op.policy?.clock === "EXECUTION_CLOCK_REPLAY" || op.policy?.clock === 1
          ? 0
          : Math.max(0, Math.floor((Date.now() - num(op.clockUnixMs)) / 1000)))
    : 0;
}
const colors = ["#18BFEA", "#39CEE7", "#77DDEB", "#40C4DA"];
export let scheduledMap = null;
export function matrixSummary(meta) {
  const q = meta?.matrixQuality;
  if (!q) return "矩阵来源未记录";
  const count = num(q.entries),
    percent = (n) => (count ? `${((100 * num(n)) / count).toFixed(1)}%` : "0%");
  return `${meta.origin?.historicalValuesUnverified ? "历史导入" : "输入值"} ${percent(q.suppliedEdges)} · 估算 ${percent(q.estimatedEdges)}${num(q.unreachableEdges) ? " · 不可达 " + num(q.unreachableEdges) + " 边" : ""}`;
}
export function schedule(view, ctx, { readonly = false } = {}) {
  scheduledMap = { view, ctx, readonly };
  const p = view.input?.problem || {},
    display = view.display || {},
    allAgents = list(p.agents),
    routes = list(view.routes),
    baseline = ctx.compare ? list(view.baseline) : [],
    op = view.op;
  const facts = new Map(list(op?.tickets).map((t) => [t.ticketId, t]));
  const changedTickets = new Set(
    ctx.compare ? changes(baseline, routes).map((c) => c.id) : [],
  );
  const routeMap = new Map(routes.map((r) => [r.agentId, r])),
    oldMap = new Map(baseline.map((r) => [r.agentId, r])),
    tickets = new Map(
      [...list(view.business?.problem?.tickets), ...list(p.tickets)].map(
        (t) => [t.id, t],
      ),
    );
  const search = (ctx.filter || "").toLowerCase();
  const selected = ctx.mapOverview || ctx.selectedTicket && !assignments(routes).has(ctx.selectedTicket) ? "" : ctx.selectedAgent || allAgents[0]?.id;
  const compact = ctx.scheduleDensity ? ctx.scheduleDensity === "compact" : allAgents.length >= 20;
  const agents = allAgents.filter(
    (a) =>
      (!ctx.resourceFilter || a.id === ctx.resourceFilter) &&
      (!search ||
        `${a.id} ${label(display, "agents", a.id)}`
          .toLowerCase()
          .includes(search) ||
        list(routeMap.get(a.id)?.stops).some((s) =>
          `${s.ticketId} ${label(display, "tickets", s.ticketId)} ${display.tickets?.[s.ticketId]?.address || ""}`
            .toLowerCase()
            .includes(search),
        )),
  );
  const allStops = [...routes, ...baseline].flatMap((r) => list(r.stops));
  const times = [
    ...allStops.flatMap((s) => [num(s.arrival), num(s.start), num(s.end)]),
    ...routes.map((r) => num(r.startTime)),
    ...routes.map((r) => num(r.endTime)).filter(Boolean),
  ];
  for (const t of list(op?.tickets))
    if (t.serviceStartedAt !== undefined) {
      times.push(num(t.serviceStartedAt), num(t.finishedAt ?? nowAt(op)));
    }
  let start = times.length
    ? Math.floor(Math.min(...times) / 1800) * 1800
    : Math.min(28800, ...allAgents.map((a) => num(a.startTime)));
  let end = times.length
    ? Math.ceil(Math.max(...times) / 1800) * 1800
    : start + 28800;
  if (ctx.fullDay) {
    start = Math.floor(start / 86400) * 86400;
    end = Math.max(start + 86400, end);
  }
  end = Math.max(start + 7200, end);
  const width = Math.round((ctx.timelineWidth || 880) * (ctx.zoom || 1)),
    rowHeight = compact ? baseline.length ? 44 : op ? 32 : 24 : baseline.length ? 88 : op ? 78 : 65,
    barHeight = compact ? 16 : 23,
    x = (s) =>
      Math.max(0, Math.min(width, ((num(s) - start) / (end - start)) * width));
  const desired = (end - start) / Math.max(3, Math.floor(width / 90)),
    step =
      [300, 600, 900, 1800, 3600, 7200, 14400, 28800, 86400].find(
        (v) => v >= desired,
      ) || 86400;
  let ticks = "",
    grid = "";
  for (let t = Math.ceil(start / step) * step; t < end; t += step) {
    const at = x(t);
    ticks += `<text x="${at + 7}" y="24" fill="#8190a2" font-size="10">${stamp(t)}</text>`;
    grid += `<line x1="${at}" x2="${at}" y1="0" y2="${rowHeight}" stroke="#edf1f6"/>`;
  }
  const current = op ? nowAt(op) : null,
    nowLine =
      current !== null && current >= start && current <= end
        ? `<line x1="${x(current)}" x2="${x(current)}" y1="0" y2="${rowHeight}" stroke="#cf7754" stroke-dasharray="3 3"/>`
        : "";
  const rect = (a, b, y, h, fill, extra = "") =>
    `<rect x="${x(a)}" y="${y}" width="${Math.max(2, x(b) - x(a))}" height="${h}" rx="3" fill="${fill}" ${extra}/>`;
  let rows = "";
  for (const agent of agents) {
    const index = allAgents.findIndex((a) => a.id === agent.id),
      color = colors[index % colors.length],
      route = routeMap.get(agent.id),
      old = oldMap.get(agent.id),
      vehicle = list(op?.vehicles).find((v) => v.agentId === agent.id),
      stops = list(route?.stops);
    let svg = grid;
    const y = compact ? baseline.length ? 19 : 4 : baseline.length ? 37 : 18;
    if (old)
      for (const s of list(old.stops))
        svg += `<g><title>原安排 ${esc(label(display, "tickets", s.ticketId))} ${stamp(s.start)}—${stamp(s.end)}</title>${rect(s.start, s.end, compact ? 4 : 12, compact ? 5 : 9, "#bbc5d1")}</g>`;
    let departure = route?.startTime ?? agent.startTime ?? stops[0]?.arrival;
    for (const [i, s] of stops.entries()) {
      const t = tickets.get(s.ticketId),
        late = t && num(s.start) > num(t.latestStart),
        chosen = ctx.selectedTicket === s.ticketId,
        locked =
          t?.pinned ||
          list(op?.routeCommitments).some((c) =>
            list(c.prefix).includes(s.ticketId),
          ) ||
          list(op?.timeCommitments).some((c) => c.ticketId === s.ticketId);
      if (num(s.arrival) > num(departure))
        svg += rect(departure, s.arrival, y + (compact ? 5 : 7), compact ? 6 : 9, "#d7f1f7");
      if (num(s.start) > num(s.arrival))
        svg += rect(s.arrival, s.start, y + (compact ? 3 : 4), compact ? 10 : 15, "#f2e3b9");
      const fact = facts.get(s.ticketId),
        done = fact && [4, 5].includes(tp(fact));
      const title = `${label(display, "tickets", s.ticketId)} · ${stamp(s.start)}—${stamp(s.end)}${fact ? " · " + phaseNames[tp(fact)] : ""}${late ? " · 超出预约时间" : ""}${locked ? " · 已锁定" : ""}`;
      const textColor = done || late ? "white" : "#163A48";
      svg += `<g data-select-ticket="${esc(s.ticketId)}" data-agent="${esc(agent.id)}" tabindex="0" role="button" aria-pressed="${chosen}" aria-label="${esc(title)}"><title>${esc(title)}</title>${rect(s.start, s.end, y, barHeight, done ? "#4D8064" : late ? "#B84838" : changedTickets.has(s.ticketId) ? "#FF992F" : color, chosen ? 'stroke="#FF7900" stroke-width="3"' : "")}`;
      if (x(s.end) - x(s.start) > 20)
        svg += `<text x="${x(s.start) + 5}" y="${y + (compact ? 11 : 15)}" fill="${textColor}" font-size="10" pointer-events="none">${locked ? "◆ " : ""}${i + 1}</text>`;
      if (x(s.end) - x(s.start) > 95)
        svg += `<text x="${x(s.start) + 26}" y="${y + (compact ? 11 : 15)}" fill="${textColor}" font-size="10" pointer-events="none">${esc(label(display, "tickets", s.ticketId).slice(0, 10))}</text>`;
      svg += "</g>";
      departure = s.end;
    }
    if (route?.endTime && num(route.endTime) > num(departure))
      svg += `<g><title>返回终点 · ${stamp(route.endTime)}</title>${rect(departure, route.endTime, y + (compact ? 5 : 7), compact ? 6 : 9, "#dfeaf0")}</g>`;
    for (const t of list(op?.tickets).filter(
      (t) => t.agentId === agent.id && t.serviceStartedAt !== undefined,
    )) {
      const finish = t.finishedAt ?? current;
      svg += `<g data-select-ticket="${esc(t.ticketId)}" data-agent="${esc(agent.id)}" tabindex="0" role="button" aria-label="实际服务 ${esc(t.ticketId)}"><title>实际服务 ${esc(label(display, "tickets", t.ticketId))} ${stamp(t.serviceStartedAt)}—${t.finishedAt !== undefined ? stamp(t.finishedAt) : "进行中"}</title>${rect(t.serviceStartedAt, Math.max(num(t.serviceStartedAt) + 1, num(finish)), rowHeight - (compact ? 8 : 18), compact ? 5 : 9, tp(t) === 4 ? "#4D8064" : "#dfa94c")}</g>`;
    }
    svg += nowLine;
    rows += `<button class="resource-cell ${selected === agent.id ? "selected" : ""}" data-select-agent="${esc(agent.id)}"><strong>${esc(label(display, "agents", agent.id))}</strong><small>${stops.filter((s) => !s.ticketId.startsWith("@vrp/")).length} 单${op ? " · " + vehicleNames[vp(vehicle)] : " · " + stamp(agent.startTime) + " 起"}</small>${vehicle?.acknowledgedPlan !== op?.plan?.version && op?.plan ? "<small>新安排待确认</small>" : ""}</button><svg class="route-row ${selected === agent.id ? "selected" : ""}" data-agent-row="${esc(agent.id)}" width="${width}" height="${rowHeight}" viewBox="0 0 ${width} ${rowHeight}" aria-label="${esc(label(display, "agents", agent.id))} 排程">${svg}${!stops.length ? `<text x="22" y="${compact ? 16 : 34}" fill="#a0adbb" font-size="11">暂无后续安排</text>` : ""}</svg>`;
  }
  const showInspector = ctx.tab !== "history" && !ctx.inspectorClosed;
  const inspector = showInspector ? inspect(
    view,
    { ...ctx, selectedAgent: selected },
    readonly,
  ) : "";
  const heading = ctx.tab === "history" ? `<h2>执行单元排程 <span class="muted">${agents.length} 个执行单元</span></h2>` :
    `<div class="schedule-filters" role="group" aria-label="排程筛选"><select id="resource-filter" aria-label="执行单元筛选"><option value="">全部执行单元</option>${allAgents.map((agent) =>
      `<option value="${esc(agent.id)}" ${ctx.resourceFilter === agent.id ? "selected" : ""}>${esc(label(display, "agents", agent.id))}</option>`).join("")}</select><input id="schedule-filter" class="filter-input" aria-label="搜索执行单元或工单" placeholder="搜索执行单元 / 工单" value="${esc(ctx.filter || "")}"></div>`;
  const legend = `<div class="legend" role="group" aria-label="排程图例">
    <span><i class="travel" aria-hidden="true"></i>行驶</span>
    <span><i class="waiting" aria-hidden="true"></i>等待</span>
    <span><i class="service" aria-hidden="true"></i>服务</span>
    ${op ? '<span><i class="actual" aria-hidden="true"></i>实际服务</span>' : ""}
    ${baseline.length ? '<span><i class="baseline" aria-hidden="true"></i>原安排</span>' : ""}
    <span>◆ 锁定</span>
    <span><i class="changed" aria-hidden="true"></i>候选改动</span>
    <span><i class="late" aria-hidden="true"></i>超出预约</span>
  </div>`;
  return `<div class="schedule-layout"><div class="workbench ${compact ? "compact" : ""} ${showInspector ? "" : "collapsed"}"><section class="panel schedule-panel" aria-label="执行单元排程"><div class="schedule-head">${heading}<span class="schedule-count">${agents.length} 个执行单元</span><div class="actions"><button data-action="schedule-density" data-density="${compact ? "comfortable" : "compact"}" aria-pressed="${compact}" title="切换排程行高">${compact ? "紧凑" : "标准"}</button><button data-action="zoom-out" title="缩小时间轴">−</button><span class="muted">${Math.round((ctx.zoom || 1) * 100)}%</span><button data-action="zoom-in" title="放大时间轴">＋</button><button data-action="day-range" class="quiet">${ctx.fullDay ? "任务时段" : "全天"}</button><button data-action="toggle-inspector" class="quiet">${showInspector ? "收起侧栏" : "展开地图"}</button></div></div>${legend}<div class="gantt-scroll" id="gantt-scroll"><div class="gantt-grid"><div class="gantt-axis-label">执行单元</div><svg class="gantt-axis" width="${width}" height="40" viewBox="0 0 ${width} 40">${ticks}</svg>${rows}</div>${!agents.length ? empty("没有匹配的执行单元", "调整搜索词或执行单元筛选后查看排程。") : ""}</div><div class="schedule-footer"><span>${stamp(start)} — ${stamp(end)}${op ? " · 虚线为" + (readonly ? "历史时间" : "当前时间") : ""}</span><span>选择工单查看路线与业务详情</span></div></section>${showInspector ? '<div class="splitter" id="inspector-resize" role="separator" aria-label="调整地图侧栏宽度"></div>' : ""}${inspector}</div></div>`;
}
function selectedSchedule(view, ctx, route, agent) {
  if (!agent) return "";
  const stops = list(route?.stops), display = view.display || {};
  if (!stops.length) return '<div class="selected-schedule"><strong>执行单元排程</strong><p class="hint">暂无已安排工单</p></div>';
  const start = num(route?.startTime ?? agent.startTime), end = Math.max(start + 1, num(route?.endTime ?? stops.at(-1).end));
  const x = (time) => Math.max(0, Math.min(1000, (num(time) - start) / (end - start) * 1000));
  const bars = stops.map((stop) => {
    const chosen = stop.ticketId === ctx.selectedTicket, title = `${label(display, "tickets", stop.ticketId)} · ${stamp(stop.start)}—${stamp(stop.end)}`;
    const wait = num(stop.start) > num(stop.arrival) ? `<rect x="${x(stop.arrival)}" y="10" width="${Math.max(2, x(stop.start) - x(stop.arrival))}" height="12" fill="#f2e3b9"/>` : "";
    return `${wait}<g data-select-ticket="${esc(stop.ticketId)}" data-agent="${esc(agent.id)}" role="button" tabindex="0" aria-label="${esc(title)}"><title>${esc(title)}</title><rect x="${x(stop.start)}" y="5" width="${Math.max(5, x(stop.end) - x(stop.start))}" height="22" rx="2" fill="#18BFEA" ${chosen ? 'stroke="#ff7900" stroke-width="3"' : ""}/></g>`;
  }).join("");
  return `<div class="selected-schedule"><div class="selected-schedule-head"><strong>执行单元排程</strong><span>${stops.filter((stop) => !stop.ticketId.startsWith("@vrp/")).length} 工单</span></div><div class="selected-time-labels"><span>${stamp(start)}</span><span>${stamp(end)}</span></div><svg data-selected-schedule viewBox="0 0 1000 32" preserveAspectRatio="none" role="img" aria-label="${esc(label(display, "agents", agent.id))} 的排程"><rect x="0" y="13" width="1000" height="6" fill="#d7f1f7"/>${bars}</svg></div>`;
}
function inspect(view, ctx, readonly) {
  const input = view.business || view.input,
    p = input?.problem || {},
    display = view.display || {},
    op = view.op,
    routes = list(view.routes),
    selected = ctx.selectedAgent,
    route = routes.find((r) => r.agentId === selected),
    agent = list(p.agents).find((a) => a.id === selected),
    ticket = list(p.tickets).find((t) => t.id === ctx.selectedTicket),
    located = assignments(routes).get(ctx.selectedTicket),
    fact = list(op?.tickets).find((t) => t.ticketId === ctx.selectedTicket),
    vehicle = list(op?.vehicles).find((v) => v.agentId === selected);
  const location = ticket ? list(p.locations)[ticket.location || 0] : null;
  let detail = "";
  if (ticket) {
    detail = `<h3>${esc(label(display, "tickets", ticket.id))} ${fact ? badge(phaseNames[tp(fact)], tp(fact) === 4 ? "green" : tp(fact) === 3 ? "orange" : "") : badge(located ? "已安排" : "未分配", located ? "teal" : "orange")}</h3>${kv(
      [
        ["工单编号", esc(ticket.id)],
        [
          "服务地点",
          esc(
            display.locations?.[location?.id]?.address ||
              label(display, "locations", location?.id) ||
              "—",
          ),
        ],
        [
          "预约开始",
          stamp(ticket.earliestStart) + " — " + stamp(ticket.latestStart),
        ],
        ["服务时长", num(ticket.duration) / 60 + " 分钟"],
        [
          "计划执行单元",
          located ? esc(label(display, "agents", located.agentId)) : "未分配",
        ],
        [
          "预计服务",
          located ? stamp(located.start) + " — " + stamp(located.end) : "—",
        ],
        ["技能要求", esc(list(ticket.skillsRequired).join("、") || "无")],
        [
          "需求量",
          esc(
            list(ticket.demand)
              .map(
                (v, i) => `${v} ${list(p.capacityDimensions)[i]?.unit || ""}`,
              )
              .join(" / ") || "无",
          ),
        ],
      ],
    )}`;
    if (fact?.serviceStartedAt !== undefined)
      detail += `<p class="hint">实际服务 ${stamp(fact.serviceStartedAt)} — ${fact.finishedAt !== undefined ? stamp(fact.finishedAt) : "进行中"}</p>`;
    if (!readonly) {
      detail += '<div class="detail-actions">';
      if (op && !op.closed && fact && ![4, 5].includes(tp(fact)))
        detail += `<button data-action="ticket-progress" data-ticket="${esc(ticket.id)}">${["出发 / 取消", "到达 / 改道", "开始服务", "完成 / 异常", "", "", "恢复 / 取消"][tp(fact)]}</button>`;
      if (view.job?.hasResult)
        detail += `<button data-action="adjust" data-ticket="${esc(ticket.id)}">调整安排</button>`;
      detail += "</div>";
    }
  } else if (agent) {
    detail = `<h3>${esc(label(display, "agents", agent.id))}</h3>${kv([
      ["执行单元编号", esc(agent.id)],
      [
        "班次",
        stamp(agent.startTime) +
          " — " +
          (agent.endTime !== undefined ? stamp(agent.endTime) : "不限"),
      ],
      ["技能", esc(list(agent.skills).join("、") || "未设置")],
      [
        "容量",
        esc(
          list(agent.capacity)
            .map((v, i) => `${v} ${list(p.capacityDimensions)[i]?.unit || ""}`)
            .join(" / ") || "未设置",
        ),
      ],
      ...(vehicle
        ? [
            ["执行状态", esc(vehicleNames[vp(vehicle)])],
            ["位置回报", stamp(vehicle.observedAt)],
            [
              "计划确认",
              op?.plan
                ? vehicle.acknowledgedPlan === op.plan.version
                  ? "已确认当前计划"
                  : "等待执行单元确认新安排"
                : "暂无生效计划",
            ],
          ]
        : []),
    ])}`;
    if (op && !op.closed && !readonly)
      detail += `<div class="detail-actions"><button data-action="vehicle-progress" data-agent="${esc(agent.id)}">位置 / 状态</button>${op.plan ? `<button data-action="ack" data-agent="${esc(agent.id)}">人工登记确认</button>` : ""}<button data-action="lock" data-agent="${esc(agent.id)}">承诺锁定</button></div>`;
  }
  if (selected) detail += `<h3>访问顺序 <small>${list(route?.stops).length} 站</small></h3><ol class="stop-list">${
    list(route?.stops)
      .map(
        (s, i) =>
          `<li><span class="sequence">${i + 1}</span><button data-select-ticket="${esc(s.ticketId)}" data-agent="${esc(selected)}">${esc(label(display, "tickets", s.ticketId))}<small>${stamp(s.start)}${s.action === "load" ? " · 装货" : s.action === "unload" ? " · 卸货" : ""}</small></button></li>`,
      )
      .join("") || '<li class="muted">暂无后续访问</li>'
  }</ol>`;
  if (!agent && !ticket) detail = `<h3>全局路线总览</h3>${kv([["执行单元", list(p.agents).length], ["业务工单", list(p.tickets).filter(t => !t.id.startsWith("@vrp/")).length], ["工作路线", routes.filter(r => list(r.stops).length).length]])}<p class="hint">地图显示全部路线。选择排程行或地图工单可查看单条路线。</p>`;
  return `<aside class="panel inspector"><div class="inspector-header"><div><h3>${esc(ticket ? label(display, "tickets", ticket.id) : selected ? label(display, "agents", selected) : "全局路线")}</h3><small>${readonly ? "历史快照 · 只读" : ticket ? located ? "已定位工单及所在路线" : "未分配工单 · 尚无执行单元路线" : selected ? "当前选中执行单元的完整路线" : `${list(p.agents).length} 个执行单元 · 全部路线`}</small></div><button data-action="clear-selection" class="quiet">全部路线</button></div>${selectedSchedule(view, ctx, route, agent)}<div class="map-frame"><div class="map-host" aria-label="路线地图"></div><div class="map-fallback">${mapSvg(view, ctx)}</div><div class="map-state" role="status">准备地图…</div><button class="map-fit quiet" type="button">定位路线</button></div><div class="map-caption">点击地图与甘特联动${ctx.compare ? " · 灰色为原安排" : ""}</div><div class="detail-body">${detail}</div></aside>`;
}
function mapSvg(view, ctx) {
  const p = view.input?.problem || {},
    business = view.business?.problem || p,
    display = view.display || {},
    all = list(p.agents),
    pointsById = new Map(
      [...list(business.locations), ...list(p.locations)].map((l) => [l.id, l]),
    ),
    routeList = list(view.routes).filter(
      (r) => !ctx.selectedAgent || r.agentId === ctx.selectedAgent,
    ),
    vehicle = list(view.op?.vehicles).find(
      (v) => v.agentId === ctx.selectedAgent,
    );
  const points = routeList.flatMap((r) =>
    list(r.stops)
      .map((s) => pointsById.get(s.locationId))
      .filter(Boolean),
  );
  if (vehicle?.position) points.push(vehicle.position);
  if (ctx.compare)
    for (const r of list(view.baseline).filter(
      (r) => !ctx.selectedAgent || r.agentId === ctx.selectedAgent,
    ))
      points.push(
        ...list(r.stops)
          .map((s) => pointsById.get(s.locationId))
          .filter(Boolean),
      );
  for (const r of routeList) {
    const a = all.find((a) => a.id === r.agentId);
    if (a && p.locations?.[a.startLocation || 0])
      points.push(p.locations[a.startLocation || 0]);
  }
  const chosen = list(business.tickets).find(
    (t) => t.id === ctx.selectedTicket,
  );
  if (chosen && business.locations?.[chosen.location || 0])
    points.push(business.locations[chosen.location || 0]);
  if (!points.length) points.push(...list(p.locations));
  const valid = points.filter(
    (l) => Number.isFinite(num(l.lng)) && Number.isFinite(num(l.lat)),
  );
  if (!valid.length)
    return '<svg class="route-map" viewBox="0 0 360 250"><text x="180" y="120" text-anchor="middle" fill="#8798aa" font-size="12">暂无位置数据</text></svg>';
  const minX = Math.min(...valid.map((l) => num(l.lng))),
    maxX = Math.max(...valid.map((l) => num(l.lng))),
    minY = Math.min(...valid.map((l) => num(l.lat))),
    maxY = Math.max(...valid.map((l) => num(l.lat))),
    dx = maxX - minX,
    dy = maxY - minY,
    scale = Math.min(290 / (dx || 0.01), 175 / (dy || 0.01));
  const xy = (l) => [
    180 + (num(l.lng) - (minX + maxX) / 2) * scale,
    125 - (num(l.lat) - (minY + maxY) / 2) * scale,
  ];
  let html =
    '<defs><marker id="route-arrow" markerWidth="7" markerHeight="7" refX="5" refY="3" orient="auto"><path d="M0,0 L6,3 L0,6" fill="#007d97"/></marker></defs><path d="M20 50H340M20 100H340M20 150H340M20 200H340M60 20V230M140 20V230M220 20V230M300 20V230" stroke="#e8edf0" fill="none"/><text x="331" y="23" fill="#92a1ad" font-size="10">N ↑</text>';
  if (ctx.compare)
    for (const r of list(view.baseline).filter(
      (r) => !ctx.selectedAgent || r.agentId === ctx.selectedAgent,
    )) {
      const pts = list(r.stops)
        .map((s) => pointsById.get(s.locationId))
        .filter(Boolean);
      html += `<polyline points="${pts.map((l) => xy(l).join(",")).join(" ")}" fill="none" stroke="#b9c3ce" stroke-dasharray="4 4" stroke-width="2"/>`;
    }
  for (const r of routeList) {
    const a = all.find((a) => a.id === r.agentId),
      index = all.findIndex((a) => a.id === r.agentId),
      color = colors[Math.max(0, index) % colors.length],
      origin =
        vehicle?.agentId === r.agentId
          ? vehicle.position
          : p.locations?.[a?.startLocation || 0];
    let previous = origin;
    for (const [i, s] of list(r.stops).entries()) {
      const l = pointsById.get(s.locationId);
      if (!l) continue;
      const [x, y] = xy(l);
      if (previous) {
        const [px, py] = xy(previous);
        html += `<line x1="${px}" y1="${py}" x2="${x}" y2="${y}" stroke="${color}" stroke-width="1.8" opacity=".65" marker-end="url(#route-arrow)"/>`;
      }
      html += `<g data-select-ticket="${esc(s.ticketId)}" data-agent="${esc(r.agentId)}" tabindex="0" role="button" aria-label="定位 ${esc(s.ticketId)}"><title>${esc(label(display, "tickets", s.ticketId))} · ${stamp(s.start)}</title><circle cx="${x}" cy="${y}" r="${ctx.selectedTicket === s.ticketId ? 12 : 9}" fill="white" stroke="${color}" stroke-width="2"/><text x="${x}" y="${y + 3}" text-anchor="middle" font-size="9" fill="${color}">${i + 1}</text></g>`;
      previous = l;
    }
    if (origin) {
      const [x, y] = xy(origin);
      html += `<rect x="${x - 5}" y="${y - 5}" width="10" height="10" rx="2" fill="${color}"/><text x="${x + (x > 180 ? -9 : 9)}" y="${y - 8}" text-anchor="${x > 180 ? "end" : "start"}" fill="${color}" font-size="10">${esc(label(display, "agents", r.agentId).slice(0, 12))}${vehicle ? " · 回报位置" : ""}</text>`;
    }
  }
  if (chosen && !assignments(view.routes).has(chosen.id)) {
    const l = business.locations?.[chosen.location || 0];
    if (l) {
      const [x, y] = xy(l);
      html += `<circle cx="${x}" cy="${y}" r="10" fill="#fff3d8" stroke="#c48b32" stroke-width="2"/><text x="${x + 14}" y="${y + 4}" font-size="10" fill="#a77c2c">未分配</text>`;
    }
  }
  return `<svg class="route-map" viewBox="0 0 360 250" role="img" aria-label="所选执行单元和工单的路线示意">${html}</svg>`;
}
