import {
  list,
  num,
  esc,
  clone,
  stamp,
  parseTime,
  label,
  score,
  scoreNumber,
  kv,
  badge,
  empty,
  tp,
  vp,
  jp,
  jk,
  phaseNames,
  vehicleNames,
  jobNames,
  kindNames,
  constraintTitle,
  reasonTitle,
  resultRoutes,
  planRoutes,
  assignments,
  changes,
  nowAt,
  schedule,
  scheduledMap,
  matrixSummary,
  icon,
} from "./schedule.js";
import {
  configureMap,
  detachMap,
  mountMap,
  mapSettings,
  invalidateGeometry,
  forgetGeometry,
  mapViewportBounds,
} from "./map.js";
import {
  field,
  select,
  check,
  grid,
  csv,
  beginDraft,
  dataPanel,
  objectEditor,
  removeObject,
  solveFields,
  readOptions,
  freshProblem,
} from "./forms.js";
import { mountSheet, checkpoint, resetSheetHistory, sheetHasErrors } from "./sheet.js";
import { mountMatrix, matrixReady } from "./matrix.js";
import { progressCharts, scoreLevels } from "./progress.js";
import { renderTaskCenter, taskActive, taskKind, taskProgress, taskState, taskStateNames, taskTone, humanBytes } from "./tasks.js";
import { renderRoadPackages } from "./roads.js";
import { renderPackageTabs, renderBasemapPackages, packageCoverage, boundsText, invalidateBasemaps } from './basemaps.js';

// Version/CAS values remain ProtoJSON strings. Each async operation captures its
// scene context; switching the UI cannot redirect an in-flight write or response.
const $ = (id) => document.getElementById(id);
const state = {
  token: "",
  view: "scene",
  sceneId: "",
  scenes: [],
  contexts: new Map(),
  health: {},
  nextScenes: "",
  sceneSearch: "",
  busy: false,
  refreshing: false,
  globalJobs: [],
  globalTasks: [],
  nextTasks: "",
  globalKind: "",
  roadPackages: [],
  packageTab: 'roads',
  basemapPackages: [],
  roadCatalog: [],
  nextJobs: "",
  globalFilter: "",
  globalScene: "",
  noticeTimer: null,
  noticeSequence: 0,
  observedPlans: new Map(),
  notifiedPlans: new Set(),
};
const ctx = () => state.contexts.get(state.sceneId);
const path = (c, action = "") =>
  `/v1/operations/${encodeURIComponent(c.id)}${action ? "/" + action : ""}`;
const action = (name, text, cls = "", extra = "") =>
  `<button data-action="${name}" class="${cls}" ${extra}>${text}</button>`;
const banner = (text, tone = "", buttons = "") =>
  `<div class="banner ${tone}"><span>${text}</span>${buttons ? `<div class="actions">${buttons}</div>` : ""}</div>`;
const active = (c) => c?.op && !c.op.closed;
const sceneName = (c) => c?.scene?.scenario.name || c?.id || "";
const currentJob = (c) =>
  c.jobs.find(
    (j) => j.id === (c.tab === "operations" ? c.liveJob : c.selectedJob),
  );
const currentPlanVersion = (c) => c.op?.plan?.version || "0";
const latestJob = (c) =>
  c.jobs.find((j) => j.id === c.op?.latestJobId) || c.jobs[0];
function sourceFresh(c, j) {
  if (
    !j?.hasResult ||
    jp(j) < 3 ||
    j.scenario?.version !== c.scene.scenario.version ||
    c.dirty
  )
    return false;
  if (c.op)
    return (
      !c.op.closed &&
      j.executionBasis?.executionVersion === c.op.executionVersion &&
      (j.executionBasis?.planVersion || "0") === currentPlanVersion(c)
    );
  return !j.executionBasis;
}
function notice(text, error = false) {
  clearTimeout(state.noticeTimer);
  state.noticeSequence++;
  $("notice-text").textContent = text;
  $("notice-icon").textContent = error ? "!" : "✓";
  $("notice").hidden = !text;
  $("notice").className = error ? "error" : "";
  $("notice").setAttribute("role", error ? "alert" : "status");
  $("notice").setAttribute("aria-live", error ? "assertive" : "polite");
  if (text)
    state.noticeTimer = setTimeout(
      () => ($("notice").hidden = true),
      error ? 12000 : 4500,
    );
}
function observePlanningTasks(tasks) {
  for (const task of list(tasks)) {
    if (taskKind(task) !== 'GEO_PLAN') continue;
    const previous = state.observedPlans.get(task.id);
    state.observedPlans.delete(task.id);
    state.observedPlans.set(task.id, task);
    if (previous && taskActive(previous) && taskState(task) === 'SUCCEEDED' && task.resultJobId && !state.notifiedPlans.has(task.id)) {
      state.notifiedPlans.add(task.id);
      const scene = state.scenes.find(s => s.id === task.scenario?.scenarioId);
      notice(`${scene?.name ? scene.name + '：' : ''}方案 #${task.resultJobId} 已就绪`);
    }
  }
  while (state.observedPlans.size > 256) state.observedPlans.delete(state.observedPlans.keys().next().value);
  while (state.notifiedPlans.size > 256) state.notifiedPlans.delete(state.notifiedPlans.values().next().value);
}
let activeRequests = 0;
const requestQueue = [];
async function api(...args) {
  if (activeRequests >= 3)
    await new Promise((resolve) => requestQueue.push(resolve));
  else activeRequests++;
  try {
    return await httpRequest(...args);
  } finally {
    if (requestQueue.length) requestQueue.shift()();
    else activeRequests--;
  }
}
async function httpRequest(url, body, accept = "application/json", method) {
  const response = await fetch(url, {
    method: method || (body === undefined ? "GET" : "POST"),
    headers: {
      Accept: accept,
      ...(body === undefined ? {} : { "Content-Type": "application/json" }),
      ...(state.token ? { Authorization: `Bearer ${state.token}` } : {}),
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  if (!response.ok) {
    let e;
    try {
      e = await response.json();
    } catch {
      e = { message: response.statusText };
    }
    const error = new Error(
      `${response.status === 409 ? "状态已变化或操作不满足条件，请刷新后重试。 " : response.status === 401 ? "请在访问设置中填写服务令牌。 " : ""}${e.message || e.code}`,
    );
    error.status = response.status;
    throw error;
  }
  return accept === "application/json" ? response.json() : response.blob();
}
function rememberView(c) {
  const g = $("gantt-scroll");
  if (c && g && g.getClientRects().length && c === state.renderedContext) {
    c.scrollX = g.scrollLeft;
    c.scrollY = g.scrollTop;
  }
  if (c && c === state.renderedContext) {
    c.detailState ||= new Map();
    document
      .querySelectorAll("#workspace-content details[data-state-key]")
      .forEach((d) => c.detailState.set(d.dataset.stateKey, d.open));
  }
}
function routeHash(c) {
  if (c)
    history.replaceState(
      null,
      "",
      `#scene/${encodeURIComponent(c.id)}/${c.tab}${c.tab === "plans" && c.selectedJob ? "/" + encodeURIComponent(c.selectedJob) : ""}`,
    );
  else history.replaceState(null, "", `#${state.view}${state.view === 'tasks' && state.taskDetail ? '/' + encodeURIComponent(state.taskDetail.id) : state.view === 'roads' ? state.packageTab==='basemaps' ? '/basemaps' + (state.basemapDetail ? '/' + encodeURIComponent(state.basemapDetail.id) : '') : state.roadDetail ? '/' + encodeURIComponent(state.roadDetail.id) : '' : ''}`);
}
function context(id) {
  if (!state.contexts.has(id))
    state.contexts.set(id, {
      id,
      scene: null,
      op: null,
      jobs: [],
      cache: new Map(),
      trends: new Map(),
      resultTabs: {},
      tab: "",
      dataTab: "tickets",
      zoom: 1,
      compare: false,
      liveJob: "",
      selectedJob: "",
      dirty: false,
      requestSerial: 0,
      historyEntries: [],
    });
  return state.contexts.get(id);
}
async function loadScenes(more = false) {
  const wanted = state.scenes.length, scenes = [];
  let cursor = more ? state.nextScenes : "";
  do {
    const page = await api(`/v1/scenarios?page_size=100${cursor ? "&after_id=" + encodeURIComponent(cursor) : ""}`);
    scenes.push(...list(page.scenarios));
    cursor = page.nextAfterId || "";
  } while (!more && cursor && scenes.length < wanted);
  state.scenes = more ? [...state.scenes, ...scenes] : scenes;
  state.nextScenes = cursor;
  renderSidebar();
}
async function loadScene(c) {
  if (c.deleted) return;
  const serial = ++c.requestSerial;
  const [meta, operation, jobs, tasks] = await Promise.all([
    api(`/v1/scenarios/${encodeURIComponent(c.id)}/metadata`),
    api(path(c)).catch((e) => {
      if (e.status === 404) return null;
      throw e;
    }),
    api(`/v1/jobs?page_size=40&scenario_id=${encodeURIComponent(c.id)}`),
    api(`/v1/tasks?page_size=40&scenario_id=${encodeURIComponent(c.id)}`),
  ]);
  if (serial !== c.requestSerial || c.deleted) return;
  const saved =
    c.scene?.scenario.version === meta.version
      ? { ...c.scene, scenario: meta }
      : await api(
          `/v1/scenarios/${encodeURIComponent(c.id)}?version=${meta.version}`,
        );
  if (serial !== c.requestSerial || c.deleted) return;
  // A business update can race these reads; retry the snapshot as one unit.
  if (operation && operation.scenario.version !== saved.scenario.version) {
    if (!c.loadingRetry) {
      c.loadingRetry = true;
      try {
        return await loadScene(c);
      } finally {
        c.loadingRetry = false;
      }
    }
    return;
  }
  c.scene = saved;
  c.op = operation;
  const oldGeo = c.geoTask;
  c.tasks = list(tasks.tasks);
  observePlanningTasks(c.tasks);
  c.geoTask = c.tasks.find(t => taskKind(t) === 'GEO_PLAN' && (!c.followGeoTaskId || t.id === c.followGeoTaskId)) || c.tasks.find(t => taskKind(t) === 'GEO_PLAN');
  if (!c.selectedJob && !c.followGeoTaskId && c.geoTask && taskActive(c.geoTask)) c.followGeoTaskId = c.geoTask.id;
  if (c.geoTask?.resultJobId && oldGeo?.revision !== c.geoTask.revision) invalidateGeometry(c.geoTask.resultJobId);
  if (c.liveJob && c.liveJob === operation?.plan?.jobId) {
    c.liveJob = "";
    c.compare = false;
  }
  if (!c.dirty && c.draftVersion !== saved.scenario.version) {
    c.draft = null;
    resetSheetHistory(c);
    delete c.rawDraft;
    c.draftVersion = saved.scenario.version;
  }
  const oldIds = new Set(list(jobs.jobs).map((j) => j.id));
  c.jobs = [
    ...list(jobs.jobs),
    ...c.jobs.filter((j) => !oldIds.has(j.id)),
  ].slice(0, 400);
  c.nextJobs = jobs.nextBeforeId || "";
  if (c.geoTask?.resultJobId && (c.followGeoTaskId === c.geoTask.id || !c.selectedJob)) {
    c.selectedJob = c.geoTask.resultJobId;
    if (!c.jobs.some(j => j.id === c.selectedJob)) c.jobs.unshift(await api(`/v1/jobs/${encodeURIComponent(c.selectedJob)}`));
    if (c.followGeoTaskId === c.geoTask.id) c.tab = 'plans';
    if (!taskActive(c.geoTask)) c.followGeoTaskId = '';
  }
  if (!c.tab)
    c.tab = operation ? "operations" : c.jobs.length ? "plans" : "data";
  if (!c.selectedJob && !(c.followGeoTaskId && c.geoTask && taskActive(c.geoTask)))
    c.selectedJob = c.jobs.find((j) => jp(j) < 3)?.id || c.jobs.find((j) => j.hasResult)?.id || c.jobs[0]?.id || "";
  const i = state.scenes.findIndex((s) => s.id === c.id);
  if (i >= 0) state.scenes[i] = saved.scenario;
  else state.scenes.unshift(saved.scenario);
  const wanted = [c.selectedJob, c.liveJob, c.baselineJob, c.op && jp(latestJob(c)) < 3 ? latestJob(c)?.id : ""].filter(Boolean);
  for (const id of new Set(wanted)) {
    const job = c.jobs.find((j) => j.id === id);
    if (job) await Promise.all([job.hasResult ? loadResult(c, job) : null, loadTrend(c, job)]);
  }
}
async function loadTrend(c, job) {
  if (c.trends.get(job.id)?.revision === job.revision) return c.trends.get(job.id);
  let trend;
  try { trend = await api(`/v1/jobs/${encodeURIComponent(job.id)}/trend`); }
  catch (error) { if (c.deleted || c.deletedJobIds?.has(job.id)) return; throw error; }
  if (c.deleted || c.deletedJobIds?.has(job.id)) return;
  c.trends.delete(job.id);
  c.trends.set(job.id, trend);
  while (c.trends.size > 8) c.trends.delete(c.trends.keys().next().value);
  return trend;
}
async function loadResult(c, job) {
  if (c.deletedJobIds?.has(job.id)) return;
  if (c.cache.has(job.id)) return c.cache.get(job.id);
  let input, result;
  try {
    [input, result] = await Promise.all([
      api(`/v1/jobs/${job.id}/request`), api(`/v1/jobs/${job.id}/result`),
    ]);
  } catch (error) { if (c.deleted || c.deletedJobIds?.has(job.id)) return; throw error; }
  if (c.deleted || c.deletedJobIds?.has(job.id)) return;
  const version = job.scenario?.version;
  const saved =
    version && version !== c.scene.scenario.version
      ? await api(
          `/v1/scenarios/${encodeURIComponent(c.id)}?version=${version}`,
        )
      : c.scene;
  if (c.deleted || c.deletedJobIds?.has(job.id)) return;
  const value = {
    input,
    result,
    business: saved.request,
    display: saved.scenario.display || {},
  };
  c.cache.set(job.id, value);
  state.resultOrder ||= [];
  state.resultOrder = state.resultOrder.filter(
    (item) => item.c !== c || item.id !== job.id,
  );
  state.resultOrder.push({ c, id: job.id });
  while (state.resultOrder.length > 6) {
    const oldest = state.resultOrder.shift();
    oldest.c.cache.delete(oldest.id);
  }
  return value;
}
async function openScene(id, tab, job) {
  rememberView(ctx());
  const c = context(id);
  state.sceneId = id;
  state.view = "scene";
  if (tab) c.tab = tab;
  if (job) c.selectedJob = job;
  render();
  await loadScene(c);
  if (c.tab === "history" && c.op) {
    await loadHistory(c);
    if (!c.historySnapshot && c.historyEntries[0])
      await selectHistory(c, c.historyEntries[0].revision);
  }
  if (job && !c.jobs.some((j) => j.id === job)) {
    c.jobs.push(await api(`/v1/jobs/${job}`));
    if (c.jobs.at(-1).hasResult) await loadResult(c, c.jobs.at(-1));
    await loadTrend(c, c.jobs.at(-1));
  }
  if (state.sceneId === id && state.view === "scene") {
    routeHash(c);
    render();
  }
  return c;
}
function refreshBlocked() {
  return state.busy || $("dialog").open || document.querySelector('.sheet-choice') ||
    document.querySelector('.scene-more[open]') ||
    document.activeElement?.matches("input,textarea,select");
}
function canRefreshProgress() {
  const c = ctx();
  return state.view === "scene" && ["plans", "operations"].includes(c?.tab) &&
    !!document.querySelector(".progress-panel") && !state.busy && !$("dialog").open &&
    !document.querySelector('.sheet-choice,.scene-more[open]');
}
function updateProgressPanels(c) {
  if (!canRefreshProgress()) return;
  document.querySelectorAll(".progress-panel").forEach((panel) => {
    const job = c.jobs.find((j) => j.id === panel.dataset.progressJob);
    if (!job) return;
    const template = document.createElement("template");
    template.innerHTML = progressPanel(c, job, panel.dataset.progressCompact === "true");
    for (const selector of ["[data-progress-heading]", "[data-progress-stats]", "[data-progress-body]"]) {
      panel.querySelector(selector).replaceWith(template.content.querySelector(selector));
    }
  });
  const output = document.querySelector("[data-plan-output]"), job = currentJob(c);
  if (c.tab === "plans" && output && job && num(output.dataset.planState) < 3 && jp(job) >= 3) {
    output.innerHTML = planOutput(c, job);
    output.dataset.planState = jp(job);
    bindSplitter(c);
    if (document.querySelector(".map-frame") && scheduledMap)
      mountMap(scheduledMap.view, scheduledMap.ctx, scheduledMap.readonly);
  }
}
function viewKey() {
  const c = ctx();
  return JSON.stringify([state.view, state.view === "scene" ?
    [c?.id, c?.tab, c?.tab === "data" ? c.dataTab : ""] : null]);
}
function renderStatus() {
  const c = ctx();
  // Compare server state without serializing the frozen requests or matrices.
  if (state.view === "scene")
    return JSON.stringify([state.view, c?.id, c?.scene?.scenario.version,
      c?.op?.revision, c?.jobs, c?.geoTask, c?.scene?.scenario.geo]);
  if (state.view === "tasks")
    return JSON.stringify([state.view, state.globalTasks, state.nextTasks, state.taskDetail]);
  if (state.view === "roads")
    return JSON.stringify([state.view, state.packageTab, state.roadPackages, state.roadDetail, state.basemapPackages, state.basemapDetail]);
  return JSON.stringify([state.view, state.health, state.scenes, mapSettings()]);
}
async function refresh({ background = false } = {}) {
  if (state.refreshing || state.busy) return;
  state.refreshing = true;
  const target = viewKey(), view = state.view, c = ctx();
  try {
    const [health] = await Promise.all([api("/healthz"), loadScenes()]);
    state.health = health;
    if (view === "scene" && c) await loadScene(c);
    else if (view === "tasks") await loadGlobalJobs();
    else if (view === "roads") await loadRoadPackages();
    renderHealth();
    // The user may have navigated or started editing while the requests ran.
    if (viewKey() !== target) return;
    if (background && refreshBlocked()) {
      updateProgressPanels(c);
      return;
    }
    const ticking = state.view === "scene" && c?.tab === "operations" &&
      active(c) && !["EXECUTION_CLOCK_REPLAY", 1].includes(c.op.policy?.clock);
    if (!background || ticking || state.renderedStatus !== renderStatus()) render();
    else if (state.view === "scene" && c?.scene) renderSceneSubtitle(c);
  } finally {
    state.refreshing = false;
  }
}
async function run(fn, message = "操作已完成") {
  if (state.busy) return;
  state.busy = true;
  const c = ctx();
  try {
    await fn();
    if (c?.scene) await loadScene(c);
    if (message) notice(message);
  } catch (e) {
    notice(e.message, true);
    if (c?.scene) await loadScene(c).catch(() => {});
  } finally {
    state.busy = false;
    render();
  }
}
function modal(
  title,
  html,
  fn,
  { submit = "确定", context: c = ctx(), after, hint = "", compact = false, readOnly = false, reload = true, destructive = false, message = "操作已完成" } = {},
) {
  $("dialog").classList.toggle("compact", compact);
  $("dialog-cancel").hidden = readOnly;
  $("dialog-title").textContent = title;
  $("dialog-title").title = title;
  $("dialog-context").textContent = c ? sceneName(c) : "vrp-1";
  $("dialog-context").title = $("dialog-context").textContent;
  $("dialog-fields").innerHTML = html;
  $("dialog-submit").textContent = submit;
  $("dialog-submit").classList.toggle("primary", !destructive);
  $("dialog-submit").classList.toggle("danger-button", destructive);
  $("dialog-hint").textContent = hint;
  $("dialog-error").hidden = true;
  $("dialog-submit").disabled = false;
  $("dialog").showModal();
  $("dialog-body").scrollTop = 0;
  $("dialog-form").onsubmit = async (event) => {
    event.preventDefault();
    if (readOnly) { $("dialog").close(); return; }
    if (state.busy) return;
    state.busy = true;
    const data = new FormData(event.currentTarget);
    const noticeAtSubmit = state.noticeSequence;
    $("dialog-submit").disabled = true;
    $("dialog-close").disabled = true;
    $("dialog-cancel").disabled = true;
    try {
      await fn(data);
      $("dialog").close();
      if (reload && c?.scene) await loadScene(c);
      if (after) await after();
      if (message && state.noticeSequence === noticeAtSubmit) notice(message);
    } catch (e) {
      $("dialog-error").textContent = e.message;
      $("dialog-error").hidden = false;
      $("dialog-body").scrollTop = $("dialog-body").scrollHeight;
      if (e.status === 409 && c?.scene) await loadScene(c).catch(() => {});
    } finally {
      state.busy = false;
      $("dialog-submit").disabled = false;
      $("dialog-close").disabled = false;
      $("dialog-cancel").disabled = false;
      render();
    }
  };
}
function download(blob, name) {
  const url = URL.createObjectURL(blob),
    a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
const scenarioStates = [
  "NEEDS_PLAN", "HAS_PLAN", "QUEUED", "SOLVING", "EXECUTING", "CLOSED", "STALE_PLAN",
];
const scenarioStateNames = [
  "待生成方案", "已有方案", "排队中", "求解中", "执行中", "已结束", "待重新规划",
];
function scenarioState(s) {
  const value = s?.summary?.state ?? 0;
  return typeof value === "number" ? value : scenarioStates.indexOf(value.replace("SCENARIO_STATE_", ""));
}
function scenarioStatus(s) {
  return s?.summary ? scenarioStateNames[scenarioState(s)] || "状态未知" : "正在读取";
}
function sceneDeleteReason(s) {
  const phase = scenarioState(s);
  return [4, 5].includes(phase) ? "此场景已有执行历史，需保留记录" :
    [2, 3].includes(phase) ? "请先停止此场景的求解任务" : "";
}
function savedTime(s, full = false) {
  const date = new Date(num(s.savedUnixMs));
  if (!num(s.savedUnixMs)) return "—";
  return full ? date.toLocaleString("zh-CN", { hour12: false }) :
    new Intl.DateTimeFormat("zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false }).format(date);
}
function sceneCounts(s) {
  return s.summary ? `${num(s.summary.ticketCount)} 工单 · ${num(s.summary.agentCount)} 执行单元` : "数量读取中";
}
function forgetScene(id) {
  const c = state.contexts.get(id);
  if (c) {
    c.deleted = true;
    c.requestSerial++;
    c.dirty = false;
    c.rawDirty = false;
  }
  state.contexts.delete(id);
  state.resultOrder = (state.resultOrder || []).filter((item) => item.c?.id !== id);
  state.globalJobs = state.globalJobs.filter((job) => job.scenario?.scenarioId !== id);
  state.scenes = state.scenes.filter((scene) => scene.id !== id);
  const selected = state.sceneId === id;
  if (selected) {
    state.sceneId = "";
    state.view = "scenes";
    routeHash();
  }
  return selected;
}
async function deleteSceneDialog(s) {
  if (!s) return;
  const meta = await api(`/v1/scenarios/${encodeURIComponent(s.id)}/metadata`);
  const reason = sceneDeleteReason(meta);
  if (reason) { notice(reason, true); return; }
  const c = state.contexts.get(s.id);
  modal(
    "删除场景",
    `<p class="form-note">确定删除 <strong>${esc(meta.name)}</strong>？场景的全部历史版本、关联求解任务及道路折线将一并删除。</p>` +
      kv([["业务数据", esc(sceneCounts(meta))], ["历史版本", num(meta.summary?.versionCount)], ["关联求解任务", num(meta.summary?.jobCount)]]) +
      (c?.dirty ? '<p class="error-text">此场景有未保存的草稿，也将丢弃。</p>' : "") +
      '<p class="form-note">删除后无法撤销。需要保留输入时，请先导出场景。</p>',
    async () => {
      await api(`/v1/scenarios/${encodeURIComponent(meta.id)}?expected_version=${encodeURIComponent(meta.version)}`, undefined, "application/json", "DELETE");
      const selected = forgetScene(meta.id);
      await loadScenes();
      if (selected && state.scenes.length) await openScene(state.scenes[0].id);
      else if (state.view === "tasks") await loadGlobalJobs();
    },
    { context: c || null, submit: "删除场景", reload: false, destructive: true, compact: true, hint: "删除全部版本及关联规划记录" },
  );
  $("dialog-context").textContent = meta.name;
  $("dialog-context").title = meta.name;
}
function planDeleteReason(c, job) {
  if (!job) return '没有选中的方案';
  if (jp(job) < 3) return '请先停止或等待本次规划完成';
  if (job.executionBasis) return '执行规划保留用于执行审计和计划回放';
  if (c.op?.plan?.jobId === job.id) return '已采用的方案保留用于执行记录';
  const derived = c.jobs.find(other => other.sourceJobId === job.id);
  if (derived) return `方案 #${derived.id} 仍引用此方案，请先删除派生方案`;
  return '';
}
function planChoice(c, job) {
  const reason = planDeleteReason(c, job);
  return `<div class="plan-job-choice">${jobChoice(c)}${action('delete-plan', '删除', 'quiet danger-button', `data-job="${esc(job.id)}" aria-label="删除方案 #${esc(job.id)}" title="${esc(reason || '删除选中的历史规划记录')}" ${reason ? 'disabled' : ''}`)}</div>`;
}
function forgetPlan(c, id) {
  c.deletedJobIds ||= new Set();
  c.deletedJobIds.add(id);
  c.requestSerial++;
  c.jobs = c.jobs.filter(job => job.id !== id);
  c.cache.delete(id);
  c.trends.delete(id);
  delete c.resultTabs[id];
  state.resultOrder = (state.resultOrder || []).filter(item => item.c !== c || item.id !== id);
  state.globalJobs = state.globalJobs.filter(job => job.id !== id);
  state.globalTasks = state.globalTasks.filter(task => task.id !== id && task.resultJobId !== id && task.solver?.id !== id);
  if (state.taskDetail?.id === id || state.taskDetail?.resultJobId === id) state.taskDetail = null;
  if (c.geoTask?.resultJobId === id) { c.geoTask = null; c.followGeoTaskId = ''; }
  if (c.selectedJob === id) { c.selectedJob = ''; c.baselineJob = ''; c.compare = false; }
  if (c.liveJob === id) c.liveJob = '';
  if (c.baselineJob === id) { c.baselineJob = ''; c.compare = false; }
  forgetGeometry(id);
}
async function deletePlanDialog(c, id) {
  if (!c?.scene || !id) return;
  let job;
  try { job = await api(`/v1/jobs/${encodeURIComponent(id)}`); }
  catch (error) {
    if (error.status !== 404) throw error;
    forgetPlan(c, id);
    await loadScene(c);
    if (state.sceneId === c.id && state.view === 'scene') routeHash(c);
    render();
    notice('该方案已删除，列表已更新');
    return;
  }
  const reason = planDeleteReason(c, job);
  if (reason) { notice(reason, true); return; }
  modal('删除历史方案',
    `<p class="form-note">确定删除方案 <strong>#${esc(job.id)}</strong>？</p>` +
    kv([['所属场景', esc(sceneName(c))], ['业务版本', 'v' + esc(job.scenario?.version || '—')], ['状态', esc(jobNames[jp(job)])]]) +
    '<p class="form-note">清理本次规划的输入、结果、趋势和道路路径，以及独占的规划任务记录。场景数据、其他方案和共享矩阵／路网仍保留。</p><p class="form-note">删除后无法撤销。</p>',
    async () => {
      try { await api(`/v1/jobs/${encodeURIComponent(job.id)}`, undefined, 'application/json', 'DELETE'); }
      catch (error) {
        const reasons = [
          ['published jobs', '该方案曾被采用，需保留执行审计记录。'],
          ['execution jobs', '执行规划需保留用于审计和计划回放。'],
          ['source jobs', '该方案仍有派生规划引用，请先处理派生方案。'],
          ['running or queued', '请先停止或等待本次规划完成，再删除。'],
        ];
        const translated = reasons.find(([text]) => error.message.includes(text));
        if (translated) error.message = translated[1];
        else if (error.status === 409 && /referenc|road snapshot/.test(error.message))
          error.message = `该方案仍被关联任务或路径引用，暂时无法删除。${error.message}`;
        if (error.status !== 404) throw error;
      }
      forgetPlan(c, job.id);
      await loadScene(c);
      if (state.sceneId === c.id && state.view === 'scene') routeHash(c);
    },
    {context:c,submit:'删除方案',destructive:true,compact:true,reload:false,message:'历史方案已删除'},
  );
}
function renderSidebar() {
  const q = state.sceneSearch.toLowerCase();
  const html =
    state.scenes
      .filter((s) => `${s.name} ${s.id}`.toLowerCase().includes(q))
      .map((s) => {
        const c = state.contexts.get(s.id);
        const selected = state.sceneId === s.id && state.view === "scene";
        return `<button data-scene="${esc(s.id)}" title="${esc(s.name)}" aria-current="${selected ? "page" : "false"}" class="${selected ? "selected" : ""}"><span class="scene-list-title"><strong>${esc(s.name)}</strong><span class="scene-list-state">${esc(scenarioStatus(s))}</span></span><small class="scene-list-counts" title="已保存的业务工单和执行单元数量">${esc(sceneCounts(s))}${c?.dirty ? " · ● 有草稿" : ""}</small><small class="scene-list-updated" title="最近保存业务数据：${esc(savedTime(s, true))}">v${esc(s.version)} · 更新 ${esc(savedTime(s))}</small></button>`;
      })
      .join("") || '<p class="hint">没有匹配的场景</p>';
  if (html !== state.sidebarMarkup) {
    const top = $("scenes").scrollTop;
    $("scenes").innerHTML = html;
    $("scenes").scrollTop = top;
    state.sidebarMarkup = html;
  }
  $("more-scenes").hidden = !state.nextScenes;
  $("more-scenes").textContent = q ? "继续加载场景以查找" : "加载更多场景";
}
function renderHealth() {
  $("health").textContent =
    `${state.health.runningJobs || 0} 运行 · ${state.health.queuedJobs || 0} 排队 · ${state.health.reservedThreads || 0}/${state.health.totalThreads || "—"} 线程`;
}
function renderSceneSubtitle(c) {
  $("scene-subtitle").hidden = !c.op;
  $("scene-subtitle").textContent = c.op
    ? `${c.op.policy?.clock === "EXECUTION_CLOCK_REPLAY" ? "回放" : "实时"} ${stamp(nowAt(c.op))}` : "";
}
function renderDraftState(c) {
  const status = $("scene-save-state"), save = $("scene-actions").querySelector('[data-action="save-draft"]');
  if (status) {
    status.textContent = c.dirty ? "● 未保存" : `已保存 v${c.scene.scenario.version}`;
    status.classList.toggle("dirty", c.dirty);
    status.title = c.dirty ? "修改保留在当前场景草稿，当前方案仍基于已保存版本" : `场景已保存为 v${c.scene.scenario.version}`;
  }
  if (save) save.disabled = !c.dirty || !!c.op?.closed;
}
function render() {
  const key = viewKey(), sameView = key === state.renderedViewKey;
  const viewport = sameView ? { x: window.scrollX, y: window.scrollY } : null;
  const matrix = sameView && document.querySelector(".matrix-scroll");
  const matrixScroll = matrix ? { x: matrix.scrollLeft, y: matrix.scrollTop } : null;
  renderWorkspace();
  state.renderedViewKey = key;
  state.renderedStatus = renderStatus();
  // Canvas sizing and map mounting can temporarily shorten the document. Restore
  // positions only after the replacement view has reached its full height.
  if (matrixScroll) {
    const replacement = document.querySelector(".matrix-scroll");
    if (replacement) {
      replacement.scrollLeft = matrixScroll.x;
      replacement.scrollTop = matrixScroll.y;
    }
  }
  if (viewport)
    window.scrollTo({ left: viewport.x, top: viewport.y, behavior: "instant" });
}
function renderWorkspace() {
  detachMap();
  const c = ctx();
  const keepMoreOpen = state.renderedContext === c && state.renderedViewKey === viewKey() &&
    !!document.querySelector('.scene-more[open]');
  rememberView(state.renderedContext);
  const focus = document.activeElement,
    focusId = focus?.id,
    start = focus?.selectionStart,
    end = focus?.selectionEnd;
  renderSidebar();
  renderHealth();
  document
    .querySelectorAll("[data-nav]")
    .forEach((b) =>
      b.classList.toggle(
        "selected",
        b.dataset.nav === (state.view === "scene" ? "scenes" : state.view),
      ),
    );
  $("initial-loading").hidden = true;
  $("global-content").hidden = state.view === "scene";
  $("scene-content").hidden = state.view !== "scene" || !c?.scene;
  state.renderedContext = state.view === "scene" ? c : null;
  if (state.view !== "scene") {
    $("global-content").innerHTML =
      state.view === "roads"
        ? renderPackageTabs(state) + (state.packageTab==='basemaps' ? renderBasemapPackages(state) : renderRoadPackages(state))
        : state.view === "tasks"
        ? globalTasks()
        : state.view === "settings"
          ? settingsPanel()
          : scenesPanel();
    if (state.view === 'roads' && state.packageTab==='basemaps' && state.basemapDetail) {
      const p=state.basemapDetail;
      mountMap({input:{problem:{locations:[],agents:[],tickets:[]}},routes:[],coverage:packageCoverage(p),basemapPackage:p},{id:'basemap-'+p.id},true);
    } else if (state.view === 'roads' && state.roadDetail) mountRoadCoverage(state.roadDetail);
    return;
  }
  if (!c?.scene) {
    $("initial-loading").hidden = false;
    return;
  }
  $("scene-context").textContent = scenarioStatus(c.scene.scenario);
  $("scene-context").classList.toggle("executing", !!active(c));
  $("scene-title").textContent = c.dirty && c.draft ? c.draft.name : sceneName(c);
  $("scene-title").title = $("scene-title").textContent;
  renderSceneSubtitle(c);
  document
    .querySelectorAll("[data-tab]")
    .forEach((b) => {
      const selected = b.dataset.tab === c.tab;
      b.classList.toggle("selected", selected);
      b.setAttribute("aria-selected", String(selected));
      b.tabIndex = selected ? 0 : -1;
    });
  $("workspace-content").setAttribute("aria-labelledby", `workspace-tab-${c.tab}`);
  const job = currentJob(c),
    fresh = sourceFresh(c, job);
  let buttons = '<span id="scene-save-state" class="scene-save-state" role="status"></span>';
  if (c.tab === "data" || c.dirty)
    buttons += action("save-draft", "保存", "", `title="${c.op ? "保存并更新执行场景" : "保存当前场景草稿"}"`);
  if (!c.op && ['data', 'plans'].includes(c.tab))
    buttons += action('geo-plan', c.jobs.length ? '生成道路矩阵并重新规划' : '生成道路矩阵并规划', 'primary', c.geoTask && taskActive(c.geoTask) ? 'disabled title="此场景已有路网规划流程正在运行"' : '');
  if (c.tab === "data" && c.op)
    buttons +=
      action(
        "solve",
        "实时重排",
        "primary",
        c.op.closed ? "disabled" : "",
      );
  else if (c.tab === "plans")
    buttons +=
      action(
        "continue",
        "继续优化",
        "",
        fresh
          ? ""
          : `disabled title="需要当前业务与执行状态下的已完成结果；草稿先保存或还原"`,
      ) + (c.op ? action(
        "solve",
        "实时重排",
        "primary",
        c.op.closed ? "disabled" : "",
      ) : "");
  else if (c.tab === "operations" && active(c))
    buttons +=
      action("new-ticket", "新增工单") +
      action("solve", "实时重排", "primary");
  const more = action("refresh", "刷新") + action("export-scene", "导出场景") +
    action("scene-properties", "场景设置") +
    (!c.op ? action('solve-existing', '仅按现有矩阵规划', '', !matrixReady(c.draft?.request || c.scene.request) ? 'disabled title="在途矩阵未就绪，请先生成道路矩阵并规划"' : 'title="仅使用已保存矩阵求解，不生成道路路径"') : '') +
    (active(c) ? action("policy", "持续重排设置") : "") +
    (c.dirty ? '<hr>' + action("discard-draft", "还原草稿") : "") +
    '<hr>' + action("delete-scene", "删除场景", "danger-button", sceneDeleteReason(c.scene.scenario) ? `disabled title="${esc(sceneDeleteReason(c.scene.scenario))}"` : "");
  $("scene-actions").innerHTML = buttons + `<details class="scene-more" ${keepMoreOpen ? "open" : ""}><summary id="scene-more-toggle" aria-label="更多场景操作" title="更多场景操作">⋯</summary><div class="scene-more-menu">${more}</div></details>`;
  renderDraftState(c);
  let statuses = "";
  if (c.geoTask && (taskActive(c.geoTask) || ['FAILED', 'PARTIAL', 'INTERRUPTED'].includes(taskState(c.geoTask))))
    statuses += `<div class="banner geo-workflow-status ${taskState(c.geoTask) === 'FAILED' ? 'warning' : ''}"><div><strong>路网规划 #${esc(c.geoTask.id)} · ${esc(taskStateNames[taskState(c.geoTask)])}</strong>${taskProgress(c.geoTask)}${c.geoTask.error ? `<span>${esc(c.geoTask.error.message || c.geoTask.error.code)}</span>` : ''}</div><div class="actions"><button data-action="open-generic-task" data-task="${esc(c.geoTask.id)}">任务详情</button>${taskActive(c.geoTask) ? `<button data-action="cancel-generic-task" data-task="${esc(c.geoTask.id)}">停止</button>` : `<button data-action="retry-generic-task" data-task="${esc(c.geoTask.id)}">重试</button>`}</div></div>`;
  else if (!c.op && c.scene.scenario.geo?.message && !['SUCCEEDED','READY'].includes(String(c.scene.scenario.geo.status || '').replace('TASK_STATE_', '').toUpperCase()))
    statuses += `<div class="banner geo-readiness"><span>${esc(c.scene.scenario.geo.message)}</span><button data-nav="roads">路网包管理</button></div>`;
  if (c.dirty && c.draftVersion !== c.scene.scenario.version)
    statuses += banner(
      "服务端业务版本已更新，当前草稿仍被保留。请导出草稿或还原后重新编辑，保存时会拒绝覆盖。",
      "warning",
    );
  if (c.op?.closed)
    statuses += banner(
      "执行已结束，历史计划与事实保留。新一轮业务请创建或导入新场景。",
    );
  $("workspace-status").innerHTML = statuses;
  const side =
    c.tab === "history"
      ? window.innerWidth > 700
        ? 302
        : 0
      : window.innerWidth > 1150 && !c.inspectorClosed
        ? (c.inspectorWidth ||
            num(
              getComputedStyle(document.documentElement)
                .getPropertyValue("--inspector-width")
                .replace("px", ""),
            )) + 8
        : 0;
  c.timelineWidth = Math.max(
    400,
    $("workspace-content").clientWidth - side - 188,
  );
  $("workspace-content").innerHTML =
    c.tab === "data"
      ? dataPanel(c)
      : c.tab === "plans"
        ? plansPanel(c)
        : c.tab === "history"
          ? historyPanel(c)
          : operationsPanel(c);
  if (c.tab === "data" && c.dataLocation !== undefined) {
    const p = c.draft.request.problem, place = p.locations[c.dataLocation];
    if (place) {
      const refs = [...list(p.tickets).filter(t => num(t.location) === c.dataLocation).map(t => label(c.draft.display, "tickets", t.id)),
        ...list(p.agents).filter(a => num(a.startLocation) === c.dataLocation || num(a.endLocation) === c.dataLocation).map(a => label(c.draft.display, "agents", a.id))];
      $("workspace-content").insertAdjacentHTML("beforeend", `<section class="panel data-location-panel"><div class="panel-head"><div><h2>${esc(label(c.draft.display, "locations", place.id))}</h2><p>${esc(refs.join("、") || "暂无关联对象")} · ${place.lng}, ${place.lat}</p></div><button class="window-close" data-action="close-location" aria-label="关闭地点预览" title="关闭地点预览">×</button></div><div class="map-frame"><div class="map-host" aria-label="关联地点地图"></div><div class="map-fallback"><p>地点：${place.lng}, ${place.lat}</p></div><div class="map-state" role="status">准备地图…</div><button class="map-fit quiet">定位地点</button></div></section>`);
    }
  }
  document.querySelectorAll("#workspace-content details").forEach((d, i) => {
    const key = `${c.tab}:${currentJob(c)?.id || "live"}:${i}`;
    d.dataset.stateKey = key;
    if (c.detailState?.has(key)) d.open = c.detailState.get(key);
  });
  const g = $("gantt-scroll");
  if (g) {
    g.scrollLeft = c.scrollX || 0;
    g.scrollTop = c.scrollY || 0;
  }
  if (c.inspectorWidth)
    document
      .querySelector(".workbench")
      ?.style.setProperty("--inspector-width", c.inspectorWidth + "px");
  if (focusId) {
    const element = $(focusId);
    if (element) {
      element.focus({ preventScroll: true });
      if (
        start !== null &&
        start !== undefined &&
        ["text", "search"].includes(element.type)
      )
        element.setSelectionRange(start, end);
    }
  }
  bindSplitter(c);
  if (c.tab === "data") {
    mountSheet(c, { render, save: () => run(() => saveDraft(c), "场景已保存"), linkedLocation: (kind, index, field, fresh) => editLinkedLocation(c, kind, index, field, fresh) });
    mountMatrix(c, { render, locate: index => { c.dataLocation = index; render(); document.querySelector('.data-location-panel')?.scrollIntoView({ block: 'nearest' }); } });
    if (document.querySelector(".map-frame")) mountMap({ input: c.draft.request, display: c.draft.display, routes: [], locationFocus: c.dataLocation }, c, true);
  } else if (document.querySelector(".map-frame") && scheduledMap)
    mountMap(scheduledMap.view, scheduledMap.ctx, scheduledMap.readonly);
}
function scenesPanel() {
  return `<div class="page-heading"><div><div class="eyebrow">场景管理</div><h1>规划与执行场景</h1><p>每个场景维护自己的工单、执行单元、方案与执行事实。</p></div><div class="actions">${action("new-scene", "新建场景", "primary")}${action("import-scene", "导入场景")}</div></div><section class="panel"><div class="table-scroll"><table class="global-table"><thead><tr><th>场景</th><th>业务版本</th><th>当前状态</th><th>保存时间</th><th></th></tr></thead><tbody>${
    state.scenes
      .filter((s) =>
        `${s.name} ${s.id}`
          .toLowerCase()
          .includes(state.sceneSearch.toLowerCase()),
      )
      .map((s) => {
        const c = state.contexts.get(s.id);
        const blocked = sceneDeleteReason(s);
        return `<tr><td><strong>${esc(s.name)}</strong><small>${esc(sceneCounts(s))}</small><small class="matrix-summary">${esc(matrixSummary(s))}</small></td><td>v${esc(s.version)} ${c?.dirty ? badge("有草稿", "orange") : ""}</td><td>${badge(scenarioStatus(s))}</td><td>${esc(savedTime(s, true))}</td><td><div class="actions"><button data-scene="${esc(s.id)}">打开工作区 →</button>${action("delete-scene", "删除", "danger-button", `data-scenario-id="${esc(s.id)}" ${blocked ? `disabled title="${esc(blocked)}"` : ""}`)}</div></td></tr>`;
      })
      .join("") || '<tr><td colspan="5">还没有场景，请新建或导入。</td></tr>'
  }</tbody></table></div></section>`;
}
function settingsPanel() {
  const h = state.health;
  return `<div class="page-heading"><div><div class="eyebrow">服务管理</div><h1>计算额度与访问</h1><p>所有场景共享本服务的求解额度，单次任务可以申请多个线程。</p></div></div><div class="settings-grid"><section class="panel"><h2>计算额度</h2><p>以下为服务当前配置。总线程额度与最大运行数在服务启动时设置。</p>${kv(
    [
      ["总线程额度", esc(h.totalThreads || 0)],
      ["已使用线程", esc(h.reservedThreads || 0)],
      ["运行 / 排队", `${h.runningJobs || 0} / ${h.queuedJobs || 0}`],
      ["最多运行任务", esc(h.maxRunningJobs || 0)],
      ["数据占用", (num(h.payloadBytes) / 1048576).toFixed(1) + " MiB"],
      ["引擎版本", esc(h.engineVersion || "—")],
    ],
  )}<p>默认额度为可用 CPU 核心数，可通过启动参数配置 CPU 倍数或明确的总线程数。</p></section><section class="panel"><h2>地图与数据</h2><p>${esc(mapSettings())}。通过服务启动参数 --map-config 配置自有底图或 PMTiles。</p><p>路网包按场景范围选择并跨场景复用。使用“生成道路矩阵并规划”准备路网估算矩阵与道路路径，缺失部分明确采用几何估算。更多操作中的“仅按现有矩阵规划”使用已保存矩阵，不生成道路路径。</p><div class="actions"><button data-nav="roads">路网包管理</button><button data-nav="tasks">任务中心</button></div></section><section class="panel"><h2>求解与采用</h2><p>快速：总时限 30 秒。标准：总时限 120 秒。深度：连续 300 秒无改进停止。每次提交都可单独指定。</p><p>持续重排与自动采用属于各个执行场景的策略，在该场景的“执行调度”中配置。默认由人审核候选再采用。</p>${action("connect", "访问设置")}<p>访问令牌仅保存在当前页面内存。页面不模拟车辆通信；“人工登记确认”表示调度员代为记录外部确认。</p></section></div>`;
}
function metrics(items) {
  return `<div class="metrics">${items.map(([name, value, tone = ""]) => `<div class="metric ${tone}"><span>${name}</span><strong>${value}</strong></div>`).join("")}</div>`;
}
function jobChoice(c, live = false) {
  const chosen = live ? c.liveJob : c.selectedJob,
    items = c.jobs.filter(
      (j) => !live || (j.executionBasis && j.id !== c.op?.plan?.jobId),
    );
  return `<select id="${live ? "live-job" : "selected-job"}" aria-label="${live ? "查看生效或候选计划" : "选择求解结果"}">${live ? `<option value="">当前生效计划${c.op?.plan ? " v" + esc(c.op.plan.version) : "（暂无）"}</option>` : ""}${items.map((j) => `<option value="${esc(j.id)}" ${j.id === chosen ? "selected" : ""}>#${esc(j.id)} · ${kindNames[jk(j)]} · ${jobNames[jp(j)]}${c.op?.plan?.jobId === j.id ? " · 已采用" : ""}</option>`).join("")}</select>`;
}
function viewFor(c, job) {
  const cache = job ? c.cache.get(job.id) : null;
  const view = {
    input: cache?.input || c.scene.request,
    business: cache?.business || c.scene.request,
    display: cache?.display || c.scene.scenario.display || {},
    routes: cache
      ? resultRoutes(cache.input, cache.result)
      : planRoutes(c.scene.request, c.op?.plan),
    baseline: c.op
      ? planRoutes(c.scene.request, c.op.plan)
      : c.baselineJob && c.cache.has(c.baselineJob)
        ? resultRoutes(
            c.cache.get(c.baselineJob).input,
            c.cache.get(c.baselineJob).result,
          )
        : [],
    op: c.tab === "plans" ? null : c.op,
    job,
    baselineJobId: c.op?.plan?.jobId || c.baselineJob,
    result: cache?.result,
  };
  if (c.selectedTicket)
    c.selectedAgent =
      assignments(view.routes).get(c.selectedTicket)?.agentId ||
      list(c.op?.tickets).find((t) => t.ticketId === c.selectedTicket)
        ?.agentId ||
      c.selectedAgent;
  return view;
}
function selectedResultTab(c) {
  return c.resultTabs[c.tab] || "schedule";
}
function selectResultTab(c, tab) {
  if (!["schedule", "unassigned", "constraints"].includes(tab)) return;
  c.resultTabs[c.tab] = tab;
}
function solutionPanels(c, view, { readonly = false, live = false, comparison = "", showToolbar = true } = {}) {
  const selected = selectedResultTab(c), assigned = assignments(view.routes);
  const count = view.result ? list(view.result.unassigned).filter((item) => !view.input.problem.tickets[item.ticket || 0]?.id.startsWith("@vrp/")).length :
    list(view.business?.problem?.tickets).filter((ticket) => !ticket.id.startsWith("@vrp/") && !assigned.has(ticket.id) &&
      ![3, 4, 5].includes(tp(list(c.op?.tickets).find((fact) => fact.ticketId === ticket.id)))).length;
  const tabs = [["schedule", "排程", null, "gantt"], ["unassigned", "未分配工单", count, "inbox"], ["constraints", "约束解释与风险", null, "shield"]];
  const nav = `<div class="result-tabs tab-strip" role="tablist" aria-label="方案内容">${tabs.map(([key, name, badgeCount, glyph]) => `<button type="button" id="result-tab-${key}" role="tab" data-result-tab="${key}" aria-selected="${selected === key}" aria-controls="result-panel" tabindex="${selected === key ? 0 : -1}" class="${selected === key ? "selected" : ""}">${icon(glyph)}<span>${esc(name)}</span>${badgeCount !== null ? ` <span class="data-tab-count">${badgeCount}</span>` : ""}</button>`).join("")}</div>`;
  const content = selected === "schedule" ? schedule(view, c, { readonly }) + candidateDetails(c, view, "changes") :
    candidateDetails(c, view, selected);
  return (showToolbar ? toolbar(c, view, live, comparison) : "") + nav + `<div id="result-panel" role="tabpanel" aria-labelledby="result-tab-${selected}">${content}</div>`;
}
function comparisonControls(c, view, comparison = "") {
  if (selectedResultTab(c) !== "schedule") return "";
  return comparison + (view.baseline.length && view.job ? `<label><input id="compare" type="checkbox" ${c.compare ? "checked" : ""}>对比原安排</label>` : "");
}
function toolbar(c, view, live, comparison = "") {
  return `<div class="work-toolbar"><div class="actions">${jobChoice(c, live)}</div><div class="actions">${comparisonControls(c, view, comparison)}${live ? (!view.job && c.op?.plan?.jobId ? action("task-info", "任务信息", "quiet", `data-job="${esc(c.op.plan.jobId)}"`) : "") + action("cargo", "装卸 / 交接") + action("close-operation", "结束执行", "quiet") : ""}</div></div>`;
}
function plansPanel(c) {
  const j = currentJob(c);
  if (!j)
    return `<section class="panel">${empty("从业务数据生成第一版方案", "准备道路矩阵后自动规划并生成道路路径；缺失路网的部分会标明几何估算。确认排程后可采用并进入执行。", action("go-data", "检查数据") + " " + action("geo-plan", "生成道路矩阵并规划", "primary", c.geoTask && taskActive(c.geoTask) ? 'disabled' : ''))}</section>`;
  return progressPanel(c, j) + `<div data-plan-output data-plan-state="${jp(j)}">${planOutput(c, j)}</div>`;
}
function planOutput(c, j) {
  if (!j.hasResult)
    return (
      `<section class="panel"><div class="plan-overview">${planChoice(c,j)}</div>${empty(jp(j) < 3 ? "方案正在计算" : "本次没有生成方案", j.error?.message || "每次求解独立保存输入与结果，可切换场景继续处理其他业务。")}</section>`
    );
  const v = viewFor(c, j);
  if (!v.result) return '<div class="loading-small">正在读取结果…</div>';
  const count = v.routes
    .flatMap((r) => r.stops)
    .filter((s) => !s.ticketId.startsWith("@vrp/")).length;
  const unassignedCount = list(v.result.unassigned).filter((t) => !v.input.problem.tickets[t.ticket || 0]?.id.startsWith("@vrp/")).length;
  const stale = j.scenario?.version !== c.scene.scenario.version;
  const primary = c.op ? action("show-live", "进入执行调度") :
    stale ? action("geo-plan", "生成道路矩阵并重新规划", "primary", c.geoTask && taskActive(c.geoTask) ? 'disabled' : '') :
    action("start", "采用并进入执行", "primary", sourceFresh(c, j) ? "" : "disabled");
  const summary = metrics([
    ["已安排", count],
    [
      "未分配",
      `<button class="metric-link" data-action="show-unassigned" data-score-explanation title="${esc(unassignedPenalty(v, unassignedCount))}；点击查看未分配原因">${unassignedCount}</button>`,
      "attention",
    ],
    [
      "行驶时间",
      Math.round(v.routes.reduce((n, r) => n + num(r.travelTime), 0) / 60) +
        " 分",
    ],
    [
      "行驶距离",
      (v.routes.reduce((n, r) => n + num(r.travelDistance), 0) / 1000).toFixed(
        1,
      ) + " km",
    ],
  ]);
  let comparison = "";
  if (!c.op && c.jobs.some((other) => other.id !== j.id && other.hasResult))
    comparison = `<div class="comparison-note"><label>对比基准 <select id="baseline-job"><option value="">不选择</option>${c.jobs
      .filter(
        (x) =>
          x.id !== j.id &&
          x.hasResult &&
          x.scenario?.version === j.scenario?.version,
      )
      .map(
        (x) =>
          `<option value="${esc(x.id)}" ${c.baselineJob === x.id ? "selected" : ""}>#${esc(x.id)} · ${kindNames[jk(x)]}</option>`,
      )
      .join("")}</select></label></div>`;
  const compare = comparisonControls(c, v, comparison);
  let workspace = `<section class="panel plan-workspace"><div class="plan-overview">${planChoice(c,j)}${summary}${compare ? `<div class="actions plan-comparison">${compare}</div>` : ""}<div class="plan-primary-action">${primary}</div></div>`;
  if (stale)
    workspace += `<div class="plan-version-notice" role="status">${icon("alert")}<div class="plan-version-notice-text"><strong>业务数据已更新</strong><span>此方案基于业务 v${esc(j.scenario?.version)}，当前为 v${esc(c.scene.scenario.version)}，请重新规划。</span></div></div>`;
  return (
    workspace +
    solutionPanels(c, v, { showToolbar: false }) +
    '</section>'
  );
}
function unassignedPenalty(v, count) {
  if (!count) return "没有未分配工单";
  const item = list(v.result?.constraints).find((constraint) => constraint.name === "ticket_unassigned");
  if (!item) return "未分配原因见“未分配工单”页签";
  const penalties = Object.entries(scoreLevels).filter(([key]) => BigInt(item.score?.[key] || 0) !== 0n)
    .map(([key, name]) => `${scoreNumber(item.score[key])} ${name}`);
  return `${count} 未分配 → ${penalties.join(" / ") || "此项未产生罚分"}${penalties.length && !num(item.score?.hard) ? "（未计 Hard）" : ""}`;
}
function operationsPanel(c) {
  if (!c.op)
    return `<section class="panel">${empty("采用规划方案后开始执行调度", "先在“规划方案”中生成、检查并采用排程；这里记录实际出发、到达和完工，偏离计划时可实时重排。已有现场任务也可先接入执行状态。", action("go-plans", "查看规划方案", "primary") + " " + action("start-empty", "先接入执行状态", "quiet"))}</section>`;
  const o = c.op,
    j = c.liveJob ? currentJob(c) : null,
    v = viewFor(c, j),
    latest = latestJob(c),
    pending = list(o.tickets).filter((t) => ![3, 4, 5].includes(tp(t))),
    assigned = assignments(v.routes),
    unassigned = pending.filter((t) => !assigned.has(t.ticketId)).length,
    acks = list(o.vehicles).filter(
      (v) => o.plan && (v.acknowledgedPlan || "0") !== (o.plan.version || "0"),
    ).length;
  let html = metrics([
    ["待执行", pending.length],
    ["服务中", list(o.tickets).filter((t) => tp(t) === 3).length],
    ["已完成", list(o.tickets).filter((t) => tp(t) === 4).length],
    ["未安排", unassigned, "attention"],
    ["待确认执行单元", acks, acks ? "attention" : ""],
  ]);
  html += `<div class="banner ${o.planStale ? "warning" : ""}"><span><strong>${j ? "候选 #" + esc(j.id) : o.plan ? "生效计划 v" + esc(o.plan.version) : "尚无生效计划"}</strong> · 持续重排 ${o.policy?.autoReplan ? "开启" : "关闭"} · ${o.policy?.autoAdopt ? "自动采用" : "人工确认"}${o.planStale ? " · 执行事实已变化，原计划待重新校验" : ""}</span>${active(c) ? action("policy", "调整策略", "quiet") : ""}</div>`;
  if (o.planningError)
    html += banner("重排未完成：" + esc(o.planningError.message), "error");
  if (latest && jp(latest) < 3) html += progressPanel(c, latest, true);
  if (!j && latest?.hasResult && latest.id !== o.plan?.jobId)
    html += banner(
      `有可查看的候选 #${esc(latest.id)} · ${kindNames[jk(latest)]}${sourceFresh(c, latest) ? "" : " · 输入可能已过时，采用时需重新校验"}`,
      "",
      action("show-candidate", "比较候选"),
    );
  if (j) {
    if (!j.hasResult)
      return (
        html +
        toolbar(c, v, true) +
        `<section class="panel">${empty("这个任务还没有最终方案", j.error?.message || "当前生效计划继续保留，可切回查看。", action("show-published", "查看生效计划"))}</section>`
      );
    if (!v.result)
      return html + '<div class="loading-small">正在读取候选…</div>';
    html += banner(
      `候选尚未生效 · ${changes(v.baseline, v.routes).length} 张工单的执行单元、顺序或时间变化。${sourceFresh(c, j) ? "" : " 当前事实与候选输入不同，请先实时重排。"}`,
      "",
      action("continue", "继续优化", "", sourceFresh(c, j) ? "" : "disabled") +
        action(
          "adopt",
          "采用候选",
          "primary",
          sourceFresh(c, j) ? "" : "disabled",
        ) +
        action("show-published", "返回生效计划", "quiet"),
    );
  }
  return (
    html +
    solutionPanels(c, v, { readonly: o.closed, live: true })
  );
}
function progressPanel(c, j, compact = false) {
  const running = jp(j) < 3,
    p = j.progress?.search || {},
    stats = j.stats || {},
    elapsed = num(stats.elapsedMs || p.elapsedMs) / 1000;
  const trend = c.trends.get(j.id), points = list(trend?.samples);
  const mode = c.scoreMode || "auto", bestFound = stats.bestFoundMs ?? p.bestFoundMs;
  const currentScore = j.score || p.bestScore || points.at(-1)?.bestScore;
  const note = trend?.partialHistory ? "此任务的早期或部分采样未保存，仅显示已有记录。" :
    `任务曲线已保存，约每 ${num(trend?.sampleIntervalMs || 1000) / 1000} 秒采样。`;
  const chart = !trend ? '<p class="trend-empty">正在读取任务曲线…</p>' : !points.length && !running ?
    '<p class="trend-empty">本次没有求解采样。</p>' : progressCharts(points, { mode, score: currentScore, range: c.trendRange || "auto", width: Math.max(280, $("workspace-content").clientWidth - 30), elapsedMs: stats.elapsedMs || p.elapsedMs });
  const options = { auto: "自动", all: "全部级别", ...scoreLevels };
  const rangeControl = `<label class="trend-select">范围 <select id="progress-range" aria-label="趋势时间范围"><option value="auto" ${c.trendRange !== "full" ? "selected" : ""}>自动细节</option><option value="full" ${c.trendRange === "full" ? "selected" : ""}>完整过程</option></select></label>`;
  return `<section class="panel progress-panel" data-progress-job="${esc(j.id)}" data-progress-compact="${compact}"><div data-progress-heading><div class="actions"><strong>${kindNames[jk(j)]} #${esc(j.id)}</strong>${badge(jobNames[jp(j)], running ? "teal" : j.hasResult ? "green" : "orange")}<span class="spacer"></span>${score(j.score || p.bestScore)}${action("task-info", "任务信息", "quiet", `data-job="${esc(j.id)}"`)}${running ? action("cancel-job", "停止求解", "quiet", `data-job="${esc(j.id)}"`) : ""}</div>${running ? '<div class="progress-track"><div class="progress-fill indeterminate"></div></div>' : ""}</div><div class="progress-meta"><div data-progress-stats><span>${jp(j) === 0 ? "等待计算额度" : `已求解 ${elapsed.toFixed(1)} 秒`}</span>${bestFound !== undefined ? `<span>最近改善于 ${(num(bestFound) / 1000).toFixed(1)} 秒</span>` : ""}<span>${j.scenario ? "业务 v" + esc(j.scenario.version) : "独立请求"}</span></div>${!compact ? `<label class="trend-select">趋势 <select id="progress-score" aria-label="趋势评分级别">${Object.entries(options).map(([key, name]) => `<option value="${key}" ${mode === key ? "selected" : ""}>${name}</option>`).join("")}</select></label>${rangeControl}` : ""}</div><div data-progress-body>${!compact ? chart + `<p class="trend-note" data-trend-history="${trend?.partialHistory ? "partial" : "saved"}">${esc(note)}整体按 Hard → Medium → Soft 依次比较。</p>` : ""}${j.error ? `<p class="error-text">${esc(j.error.message)}</p>` : ""}</div></section>`;
}
function jobDetails(j, contentOnly = false) {
  const content = `<div class="task-detail-content">${kv(
    [
      ["任务类型", kindNames[jk(j)]],
      ["计算线程", esc(j.threads || 1)],
      ["候选评估", num(j.stats?.evaluatedMoves || j.progress?.search?.evaluatedMoves).toLocaleString()],
      ["来源任务", esc(j.sourceJobId || "无")],
      ["提交时间", new Date(num(j.createdUnixMs)).toLocaleString("zh-CN")],
      [
        "停止原因",
        esc(
          {
            time_limit: "达到总时限",
            unimproved: "达到无改进时限",
            cancelled: "主动停止",
            completed: "无需继续搜索",
            manual_evaluation: "人工候选已校验评分",
          }[j.stats?.terminationReason] ||
            j.stats?.terminationReason ||
            "尚未停止",
        ),
      ],
      ["人工调整", esc(j.reason || "—")],
    ],
  )}<div class="actions">${action("download-input", "下载冻结输入", "quiet", `data-job="${esc(j.id)}"`)}${action("download-result", "下载结果", "quiet", `data-job="${esc(j.id)}" ${j.hasResult ? "" : "disabled"}`)}${j.hasResult ? action("geometry-import", "导入道路路径", "quiet", `data-job="${esc(j.id)}"`) : ""}</div></div>`;
  return contentOnly ? content : `<details class="panel task-detail"><summary>任务详情与输入来源 · #${esc(j.id)}</summary>${content}</details>`;
}
function candidateDetails(c, v, section = "all") {
  const p = v.input.problem,
    d = v.display,
    a = assignments(v.routes),
    fact = new Map(list(c.op?.tickets).map((t) => [t.ticketId, t]));
  const unassigned = v.result
    ? list(v.result.unassigned).map((u) => ({
        t: p.tickets[u.ticket || 0],
        reasons: u.reasons,
      }))
    : list(c.scene.request.problem.tickets)
        .filter((t) => !a.has(t.id) && ![3, 4, 5].includes(tp(fact.get(t.id))))
        .map((t) => ({ t, reasons: [] }));
  let html = `<section class="result-list-panel" id="plan-unassigned"><div class="tray-head"><strong>未分配工单 ${unassigned.filter((x) => !x.t?.id.startsWith("@vrp/")).length}</strong><span class="muted">选择工单定位详情</span></div><div class="tray-body"><table><thead><tr><th>工单</th><th>预约时间</th><th>说明</th><th></th></tr></thead><tbody>${
    unassigned
      .filter((x) => x.t && !x.t.id.startsWith("@vrp/"))
      .map(
        ({ t, reasons }) =>
          `<tr><td><button class="row-link" data-select-ticket="${esc(t.id)}">${esc(label(d, "tickets", t.id))}</button><small>${esc(t.id)}</small></td><td>${stamp(t.earliestStart)} — ${stamp(t.latestStart)}</td><td>${esc(list(reasons).map(reasonTitle).join("；") || "当前计划未安排")}</td><td>${v.job?.hasResult ? action("adjust", "调整安排", "", `data-ticket="${esc(t.id)}"`) : active(c) ? action("ticket-progress", "登记状态", "", `data-ticket="${esc(t.id)}"`) : ""}</td></tr>`,
      )
      .join("") || '<tr><td colspan="4">所有待执行工单均已安排</td></tr>'
  }</tbody></table></div></section>`;
  if (section === "unassigned") return html;
  if (section !== "all") html = "";
  if (v.job && c.compare && v.baseline.length && ["all", "changes"].includes(section)) {
    const diff = changes(v.baseline, v.routes);
    html += `<section class="panel bottom-panel"><div class="panel-head"><h2>方案变化 · ${diff.length} 单</h2><span class="muted">原安排 → 候选</span></div><div class="diff-table"><table><thead><tr><th>工单</th><th>原安排</th><th>候选安排</th></tr></thead><tbody>${diff.map((x) => `<tr><td><button class="row-link" data-select-ticket="${esc(x.id)}" data-agent="${esc(x.after?.agentId || x.before?.agentId || "")}">${esc(label(d, "tickets", x.id))}</button></td>${[x.before, x.after].map((s) => `<td>${s ? `${esc(label(d, "agents", s.agentId))} · 第 ${s.order + 1} 站 · ${stamp(s.start)}` : "未安排"}</td>`).join("")}</tr>`).join("") || '<tr><td colspan="3">执行单元、顺序和开始时间未变化</td></tr>'}</tbody></table></div></section>`;
  }
  if (section === "changes") return html;
  if (section === "constraints" && !v.result)
    return `<section class="result-list-panel">${empty("当前生效计划没有附带评分明细", "可在规划方案中查看来源任务的约束解释与风险。")}</section>`;
  if (v.result)
    html += `<section class="result-list-panel" id="plan-constraints"><div class="tray-head"><strong>约束解释与风险</strong><span>${score(v.result.score)} · ${list(v.result.estimatedEdges).length} 条估算边</span></div><div class="table-scroll"><table><thead><tr><th>约束</th><th>匹配数</th><th>罚分</th><th>相关对象</th></tr></thead><tbody>${
      list(v.result.constraints)
        .filter(
          (x) =>
            num(x.matches) ||
            num(x.score?.hard) ||
            num(x.score?.medium) ||
            num(x.score?.soft),
        )
        .map((x) => {
          const matches = list(v.result.matches).filter(
            (m) => m.constraint === x.name,
          );
          const ids = [
            ...new Set(
              matches.flatMap((m) =>
                list(m.tickets)
                  .map((i) => p.tickets[i]?.id)
                  .filter(Boolean),
              ),
            ),
          ];
          return `<tr><td>${esc(constraintTitle(x.name))}</td><td>${esc(x.matches || 0)}</td><td>${score(x.score)}</td><td>${ids
            .slice(0, 6)
            .map(
              (id) =>
                `<button class="row-link" data-select-ticket="${esc(id)}" data-agent="${esc(a.get(id)?.agentId || "")}">${esc(label(d, "tickets", id))}</button>`,
            )
            .join(
              " · ",
            )}${ids.length > 6 ? " 等 " + ids.length + " 单" : ""}${ids.length ? "" : matches.flatMap((m) => list(m.agents)).length ? "执行单元级约束" : "全局约束"}</td></tr>`;
        })
        .join("") || '<tr><td colspan="4">没有带罚分的约束</td></tr>'
    }</tbody></table></div><p class="hint panel-pad">匹配表示约束的计分情况，不等于全部都是业务异常。估算边的排班时长包含余量；地图实线为外部道路路径，虚线为访问顺序示意。</p></section>`;
  return html;
}
const historyNames = {
  start: "开始执行",
  event: "执行事实回报",
  configure: "策略 / 承诺调整",
  replan: "提交实时重排",
  optimize: "继续优化",
  manual_adjustment: "人工候选调整",
  adopt: "人工采用方案",
  auto_adopt: "自动采用方案",
  acknowledge: "执行单元确认计划",
  close: "结束执行",
  business_update: "更新业务数据",
  planning_error: "重排失败",
  adoption_blocked: "候选未通过采用校验",
  recovery: "恢复执行状态",
};
function historyPanel(c) {
  if (!c.op)
    return `<section class="panel">${empty("尚无执行历史", "求解记录在“规划方案”和全局“求解任务”中。采用计划进入执行后，这里按事件展示计划与实际事实的演进。")}</section>`;
  const h = c.historySnapshot;
  return `<div class="work-toolbar"><p class="hint">按真实记录查看当时的计划与执行状态，历史视图只读。</p><div class="actions">${action("history-refresh", "刷新记录")}${action("show-live", "回到当前执行")}</div></div><div class="history-layout"><section class="panel"><div class="panel-head"><h2>事件记录</h2><span class="muted">最近优先</span></div><div class="history-events">${c.historyEntries.map((e) => `<button class="history-event ${e.revision === h?.op.revision ? "selected" : ""}" data-history="${esc(e.revision)}"><strong>${stamp(e.at)} · ${esc(historyNames[e.kind] || e.kind)}</strong><small>${esc(e.actor || "系统")} · 业务 v${esc(e.scenarioVersion)} · ${num(e.planVersion) ? "计划 v" + esc(e.planVersion) : "未采用计划"}</small><small>${esc([...list(e.ticketIds), ...list(e.agentIds)].join("、"))}</small></button>`).join("") || '<div class="loading-small">点击刷新记录加载历史</div>'}</div>${c.nextHistory ? action("history-more", "加载更早记录", "quiet wide") : ""}</section><section class="history-preview">${
    h
      ? banner(
          `${stamp(h.op.at)} · ${esc(historyNames[h.entry?.kind] || "历史快照")} · 计划 ${h.op.plan ? "v" + esc(h.op.plan.version) : "无"}`,
        ) +
        schedule(
          {
            input: h.scene.request,
            business: h.scene.request,
            display: h.scene.scenario.display || {},
            op: { ...h.op, policy: { ...h.op.policy, clock: 1 } },
            routes: planRoutes(h.scene.request, h.op.plan),
          },
          { ...c, compare: false, filter: "", resourceFilter: "" },
          { readonly: true },
        ) +
        `<section class="panel bottom-panel"><div class="panel-head"><h2>当时的执行事实</h2></div><div class="table-scroll"><table><thead><tr><th>工单</th><th>状态</th><th>执行单元</th><th>实际服务</th><th>备注</th></tr></thead><tbody>${
          list(h.op.tickets)
            .filter((t) => tp(t) !== 0)
            .map(
              (t) =>
                `<tr><td>${esc(label(h.scene.scenario.display, "tickets", t.ticketId))}</td><td>${phaseNames[tp(t)]}</td><td>${esc(t.agentId || "—")}</td><td>${t.serviceStartedAt !== undefined ? stamp(t.serviceStartedAt) : "—"} / ${t.finishedAt !== undefined ? stamp(t.finishedAt) : "—"}</td><td>${esc(t.note || "—")}</td></tr>`,
            )
            .join("") || '<tr><td colspan="5">此时尚无实际服务记录</td></tr>'
        }</tbody></table></div></section>`
      : `<section class="panel">${empty("选择一个事件", "查看当时的生效计划、执行单元安排和执行事实。")}</section>`
  }</section></div>`;
}
async function loadHistory(c, more = false) {
  const page = await api(
    `${path(c, "history")}?page_size=40${more ? "&before_revision=" + c.nextHistory : ""}`,
  );
  c.historyEntries = more
    ? [...c.historyEntries, ...list(page.entries)]
    : list(page.entries);
  c.nextHistory = page.nextBeforeRevision || "";
}
async function selectHistory(c, revision) {
  const op = await api(`${path(c)}?revision=${revision}`),
    scene = await api(
      `/v1/scenarios/${encodeURIComponent(c.id)}?version=${op.scenario.version}`,
    );
  c.historySnapshot = {
    op,
    scene,
    entry: c.historyEntries.find((e) => e.revision === revision),
  };
}
async function loadGlobalJobs(more = false) {
  const serial = (state.globalSerial = (state.globalSerial || 0) + 1);
  const page = await api(`/v1/tasks?page_size=80${state.globalScene ? '&scenario_id=' + encodeURIComponent(state.globalScene) : ''}${state.globalKind ? '&kind=' + encodeURIComponent(state.globalKind) : ''}${more && state.nextTasks ? '&before_id=' + encodeURIComponent(state.nextTasks) : ''}`);
  if (serial !== state.globalSerial) return;
  observePlanningTasks(page.tasks);
  const seen = new Set(list(page.tasks).map(t => t.id));
  state.globalTasks = more ? [...state.globalTasks.filter(t => !seen.has(t.id)), ...list(page.tasks)] : list(page.tasks);
  state.nextTasks = page.nextBeforeId || '';
  if (state.taskDetail) {
    const id = state.taskDetail.id;
    const detail = await api(`/v1/tasks/${encodeURIComponent(id)}`);
    if (serial === state.globalSerial && state.taskDetail?.id === id) state.taskDetail = detail;
  }
}
function globalTasks() { return renderTaskCenter(state); }
async function loadRoadPackages() {
  if (state.packageTab==='basemaps') {
    const packages=[];let cursor='',page;
    do{page=await api(`/v1/basemap-packages?page_size=100${cursor?'&after_id='+encodeURIComponent(cursor):''}`);packages.push(...list(page.packages));cursor=page.nextAfterId || '';}while(cursor && packages.length<1000);
    state.basemapPackages=packages;state.basemapUsed=page.usedBytes;state.basemapLimit=page.limitBytes;
    if (state.basemapDetail) state.basemapDetail=await api(`/v1/basemap-packages/${encodeURIComponent(state.basemapDetail.id)}`);
    return;
  }
  const serial = (state.roadSerial = (state.roadSerial || 0) + 1);
  const packages = [];
  let cursor = '';
  do {
    const page = await api(`/v1/road-packages?page_size=100${cursor ? '&after_id=' + encodeURIComponent(cursor) : ''}`);
    packages.push(...list(page.packages));
    cursor = page.nextAfterId || '';
  } while (cursor && packages.length < 1000);
  if (serial !== state.roadSerial) return;
  state.roadPackages = packages;
  if (state.roadDetail) {
    const id = encodeURIComponent(state.roadDetail.id);
    const [detail, versions] = await Promise.all([api(`/v1/road-packages/${id}`), api(`/v1/road-packages/${id}/versions`)]);
    if (serial !== state.roadSerial || state.roadDetail?.id !== detail.id) return;
    state.roadDetail = detail;
    state.roadVersions = list(versions.packages);
    state.roadReportCache ||= new Map();
    const snapshots = list(detail.graphArtifacts).slice(0, 10);
    const reports = await Promise.allSettled(snapshots.map(async artifact => {
      const key = `${artifact.taskId}:${artifact.sha256}`;
      if (!state.roadReportCache.has(key)) {
        state.roadReportCache.set(key, await api(`/v1/tasks/${encodeURIComponent(artifact.taskId)}/artifacts/quality`));
        if (state.roadReportCache.size > 30) state.roadReportCache.delete(state.roadReportCache.keys().next().value);
      }
      return {taskId:artifact.taskId, ...state.roadReportCache.get(key)};
    }));
    if (serial === state.roadSerial && state.roadDetail?.id === detail.id) state.roadReports = reports.filter(r => r.status === 'fulfilled').map(r => r.value);
  }
}
function mountRoadCoverage(p) {
  let coverage;
  try { coverage = JSON.parse(p.coverageGeojson || 'null'); } catch { return; }
  if (coverage?.type === 'Feature') coverage = coverage.geometry;
  if (!['Polygon','MultiPolygon'].includes(coverage?.type)) return;
  const frame = $('road-coverage-map');
  if (!frame) return;
  frame.hidden = false;
  mountMap({input:{problem:{locations:[],agents:[],tickets:[]}},routes:[],coverage}, {id:'road-' + p.id}, true);
}
async function prepareBasemapDialog({context,bounds,level=14,selection,updateId}={}) {
  if (!context && !bounds && !state.scenes.length) throw Error('请先保存一个包含经纬度的场景');
  let request,estimated;
  modal(level===15?'准备详细底图':'准备离线底图',
    (!context&&!bounds?select('已保存场景','scene',state.scenes.map(s=>[s.id,s.name]),state.sceneId):`<p>范围：${esc(boundsText(bounds || selection?.bounds) || '已保存业务快照向外缓冲 10 公里')}</p>`)+
    select('目标原生级别','level',[[14,'0–14 级'],[15,'0–15 级（详细）']],level)+
    '<p class="hint">先读取目录预估范围、实际新增传输和磁盘峰值，再确认下载。同源详细包复用已有低级别数据。放大地图不会自动追加下载。</p>',async form=>{
      request={nativeMaxZoom:Number(form.get('level')),forceUpdate:!!updateId,...(context?{context}:bounds?{bounds}:{context:{scenarioId:form.get('scene')}})};
      estimated=await api('/v1/basemap-packages/estimate',{...request,idempotencyKey:crypto.randomUUID()});
      const started=Date.now();
      while(taskActive(estimated)) {
        if(Date.now()-started>120000) throw Error(`预估仍在后台运行，请在任务中心查看 #${estimated.id}`);
        $('dialog-hint').textContent=`正在预估 #${estimated.id} · ${estimated.progress?.message || estimated.progress?.phase || '读取源目录'}…`;
        await new Promise(resolve=>setTimeout(resolve,600));
        estimated=await api(`/v1/tasks/${encodeURIComponent(estimated.id)}`);
      }
      if(!estimated.basemapEstimate) throw Error(estimated.error?.message || '预估未完成，请到任务中心查看或重试');
    },{context:null,submit:'预估新增下载量',reload:false,message:'',after:()=>{
      const e=estimated.basemapEstimate;
      modal('确认准备离线底图',`<dl class="road-metadata"><dt>范围</dt><dd>${esc(boundsText(e.bounds))}</dd><dt>原生级别</dt><dd>0–${e.nativeMaxZoom}</dd><dt>预计新增传输</dt><dd>${humanBytes(e.transferBytes)}</dd><dt>包大小上限预估</dt><dd>${humanBytes(e.packageBytes)}</dd><dt>磁盘新增峰值</dt><dd>${humanBytes(e.diskPeakBytes)}</dd><dt>固定源版本</dt><dd>${esc(e.source?.key)} · ${esc(e.source?.version)}</dd></dl><p class="hint">准备期间继续使用当前底图；新包校验通过后切换。不影响矩阵、求解或执行操作。</p>`,async()=>{
        const task=await api('/v1/basemap-packages/prepare',{...request,estimateTaskId:estimated.id,idempotencyKey:crypto.randomUUID()});
        invalidateBasemaps();
        if(task.basemapPackageId && taskState(task)==='SUCCEEDED') {
          state.view='roads';state.packageTab='basemaps';state.basemapDetail=await api(`/v1/basemap-packages/${encodeURIComponent(task.basemapPackageId)}`);await loadRoadPackages();routeHash();render();
        }else await openGenericTask(task.id);
      },{context:null,submit:'开始准备',reload:false});
    }});
}
window.addEventListener('prepare-basemap',event=>safely(()=>{
  const {context,selection,outside}=event.detail;
  return prepareBasemapDialog({context:outside?undefined:context,bounds:outside?mapViewportBounds():undefined,selection,level:selection?.package&&!outside?15:14});
}));
async function openGenericTask(id) {
  rememberView(ctx());
  const task = await api(`/v1/tasks/${encodeURIComponent(id)}`);
  state.view = 'tasks';
  state.taskDetail = task;
  await loadGlobalJobs();
  routeHash();
  render();
  document.querySelector('.generic-task-detail')?.scrollIntoView({block:'nearest'});
}
async function prepareGeoPlan(c, options = c?.scene?.request?.options) {
  if (!c?.scene) throw Error('请先打开场景');
  if (c.op) throw Error('执行场景请使用实时重排');
  if (c.geoTask && taskActive(c.geoTask)) { await openGenericTask(c.geoTask.id); return; }
  if (c.dirty) await saveDraft(c);
  const task = await api(`/v1/scenarios/${encodeURIComponent(c.id)}/geo-plan`, {
    expectedVersion:c.scene.scenario.version, idempotencyKey:crypto.randomUUID(), options,
  });
  c.geoTask = task;
  observePlanningTasks([task]);
  c.followGeoTaskId = task.id;
  c.tasks = [task,...list(c.tasks).filter(t => t.id !== task.id)];
  c.tab = 'plans';
  c.selectedJob = '';
  if (state.sceneId === c.id && state.view === 'scene') routeHash(c);
}
function bindSplitter(c) {
  const splitter = $("inspector-resize");
  if (!splitter) return;
  splitter.onpointerdown = (e) => {
    const start = e.clientX,
      width = document
        .querySelector(".inspector")
        .getBoundingClientRect().width;
    splitter.setPointerCapture(e.pointerId);
    splitter.onpointermove = (move) => {
      c.inspectorWidth = Math.max(
        260,
        Math.min(520, width + start - move.clientX),
      );
      document
        .querySelector(".workbench")
        .style.setProperty("--inspector-width", c.inspectorWidth + "px");
    };
    splitter.onpointerup = () => {
      splitter.onpointermove = null;
    };
  };
}

async function saveDraft(c) {
  beginDraft(c);
  if (sheetHasErrors(c)) throw Error("请先修正工作表中标红的单元格，或按 Esc 放弃该格修改");
  if (c.rawDirty) {
    const request = JSON.parse(c.rawDraft);
    if (!request.problem) throw Error("需要包含 problem 的 SolveRequest");
    c.draft.request = request;
    c.rawDirty = false;
  }
  const saved = await api("/v1/scenarios", {
    scenarioId: c.id,
    name: c.draft.name,
    expectedVersion: c.draftVersion,
    request: c.draft.request,
    display: c.draft.display,
  });
  c.scene = { scenario: saved, request: clone(c.draft.request) };
  c.draftVersion = saved.version;
  c.dirty = false;
  resetSheetHistory(c);
  delete c.rawDraft;
  // The server projects the updated business version into this operation.
  if (c.op) c.op = await api(path(c));
}
function newScene(importing = false) {
  const id = `scene-${Date.now().toString(36)}`;
  modal(
    importing ? "导入场景" : "新建场景",
    grid(
      field("场景名称", "name", "", "text", "required") +
        field(
          "场景编号",
          "id",
          id,
          "text",
          'required pattern="[A-Za-z0-9_-]{1,64}"',
        ),
    ) +
      (importing
        ? '<div class="form-stack"><label>原生 SolveRequest（JSON 或 protobuf）<input name="file" type="file" accept=".json,.pb" required></label><p class="hint">历史 vrp-0 数据先用 vrp-cli 转换。也支持包含 request 与 display 的本工作台导出文件。</p></div>'
        : '<p class="hint">创建后在“数据与规则”中录入工单、执行单元、地点和约束。</p>'),
    async (f) => {
      let saved;
      if (importing) {
        const file = f.get("file");
        if (file.name.endsWith(".pb")) {
          const bytes = new Uint8Array(await file.arrayBuffer());
          let binary = "";
          for (let i = 0; i < bytes.length; i += 8192)
            binary += String.fromCharCode(...bytes.subarray(i, i + 8192));
          saved = await api("/v1/scenarios/import", {
            scenarioId: f.get("id"),
            name: f.get("name"),
            requestBytes: btoa(binary),
          });
        } else {
          const value = JSON.parse(await file.text());
          saved = await api("/v1/scenarios", {
            scenarioId: f.get("id"),
            name: f.get("name"),
            request: value.request || value,
            display: value.display,
            origin: value.origin,
          });
        }
      } else
        saved = await api("/v1/scenarios", {
          scenarioId: f.get("id"),
          name: f.get("name"),
          request: freshProblem(f.get("id")),
          display: { locations: { DEPOT: { name: "中心网点" } } },
        });
      await loadScenes();
      await openScene(saved.id, "data");
    },
    { context: null, submit: importing ? "导入并打开" : "创建场景" },
  );
}
function editObject(c, kind, index, urgent = false) {
  const editor = objectEditor(c, kind, index);
  modal(
    `${index === undefined ? "新增" : "编辑"}${{ tickets: "工单", agents: "执行单元", locations: "地点" }[kind]}`,
    editor.html,
    async (f) => {
      checkpoint(c);
      editor.apply(f);
      if (urgent) await saveDraft(c);
    },
    { context: c, submit: urgent ? "保存工单" : "应用", hint: urgent ? "同时保存当前场景草稿" : "修改应用到草稿" },
  );
}
function editLinkedLocation(c, kind, index, fieldName, fresh = false) {
  beginDraft(c);
  const p = c.draft.request.problem, owner = p[kind][index], placeIndex = num(owner[fieldName]);
  const place = p.locations[placeIndex];
  const refs = list(p.tickets).filter(t => num(t.location) === placeIndex).length + list(p.agents).filter(a => num(a.startLocation) === placeIndex || num(a.endLocation) === placeIndex).length;
  const editor = objectEditor(c, "locations", fresh ? undefined : placeIndex);
  modal(`${fresh ? "新建" : "编辑"}关联地点 · ${label(c.draft.display, kind, owner.id)}`, editor.html +
    (!fresh ? grid(select("修改范围", "locationScope", [["copy", "为当前对象另建地点"], ["shared", `更新共享地点（关联 ${refs} 个对象）`]], refs > 1 ? "copy" : "shared")) : ""), f => {
      checkpoint(c);
      const copy = fresh || f.get("locationScope") === "copy";
      if (copy) {
        let n = p.locations.length + 1, id;
        do { id = 'L' + String(n++).padStart(3, '0'); } while (p.locations.some(l => l.id === id));
        f.set("id", id);
        objectEditor(c, "locations", undefined).apply(f);
        owner[fieldName] = p.locations.length - 1;
        c.dataLocation = owner[fieldName];
      } else { editor.apply(f); c.dataLocation = placeIndex; }
    }, { context: c, submit: "应用", hint: "地点和关联一并应用到草稿" });
  if (fresh && place) {
    $("dialog-fields").querySelector('[name=lng]').value = place.lng;
    $("dialog-fields").querySelector('[name=lat]').value = place.lat;
    $("dialog-fields").querySelector('[name=name]').value = `地点${String(p.locations.length + 1).padStart(3, '0')}`;
  }
}
function syncRawDraft(c) {
  if (!c.rawDirty) return;
  const request = JSON.parse(c.rawDraft);
  if (!request.problem) throw Error("原始输入需要包含 problem，请先修正再继续编辑");
  checkpoint(c);
  c.draft.request = request;
  c.rawDirty = false;
  delete c.rawDraft;
}
function sceneDate(c) {
  if (c.op) throw Error("已有执行，业务日期不可更改");
  beginDraft(c);
  syncRawDraft(c);
  const p = c.draft.request.problem;
  modal("业务日期", field("日期", "date", p.horizon?.startLocal?.slice(0, 10) || new Date().toLocaleDateString("sv-SE"), "date", "required") +
    `<p class="hint">时区：${esc(p.horizon?.timezone || "未设置，可在场景设置中配置")}</p>`, (f) => {
      const date = f.get("date");
      if (date === p.horizon?.startLocal?.slice(0, 10)) return;
      checkpoint(c);
      p.horizon = { ...p.horizon, startLocal: date + (p.horizon?.startLocal?.slice(10) || "T00:00:00") };
      c.dirty = true;
      delete c.rawDraft;
    }, { context: c, compact: true, submit: "应用", hint: "修改日期应用到当前场景草稿" });
}
function sceneProperties(c) {
  beginDraft(c);
  syncRawDraft(c);
  const p = c.draft.request.problem;
  modal(
    "场景设置",
    grid(
      field("场景名称", "name", c.draft.name, "text", "required") +
        field(
          "业务日期",
          "date",
          p.horizon?.startLocal?.slice(0, 10) ||
            new Date().toLocaleDateString("sv-SE"),
          "date",
          c.op ? "required disabled" : "required",
        ) +
        field(
          "时区",
          "timezone",
          p.horizon?.timezone || "Asia/Shanghai",
          "text",
          c.op ? "required disabled" : "required",
        ) +
        field(
          "容量维度名（逗号分隔）",
          "dimensions",
          list(p.capacityDimensions)
            .map((v) => v.name)
            .join(","),
        ) +
        field(
          "单位（逗号分隔）",
          "units",
          list(p.capacityDimensions)
            .map((v) => v.unit)
            .join(","),
        ),
    ) +
      '<p class="hint">调整容量维度时，需要同步检查执行单元容量和工单需求。已有执行的时间基准不可更改。</p>',
    (f) => {
      const names = csv(f.get("dimensions")),
        units = csv(f.get("units"));
      if (names.length !== units.length) throw Error("维度名与单位数量不一致");
      checkpoint(c);
      c.draft.name = f.get("name");
      if (!c.op) p.horizon = {
        ...p.horizon,
        startLocal: f.get("date") + "T00:00:00",
        timezone: f.get("timezone"),
      };
      p.capacityDimensions = names.map((name, i) => ({
        ...list(p.capacityDimensions)[i],
        name,
        unit: units[i],
        penaltyUnit: list(p.capacityDimensions)[i]?.penaltyUnit || "1",
      }));
      c.dirty = true;
      delete c.rawDraft;
    },
    { context: c, submit: "应用", hint: "修改应用到草稿" },
  );
}
function solveDialog(c, continuing = false, existingMatrix = false) {
  const source = currentJob(c);
  const withRoads = !continuing && !c.op && !existingMatrix;
  if (continuing && !sourceFresh(c, source))
    throw Error("原任务的输入已不是当前场景，请使用重新规划或实时重排");
  if (c.op?.closed) throw Error("场景执行已结束");
  if (withRoads && c.geoTask && taskActive(c.geoTask)) {
    openGenericTask(c.geoTask.id).catch(e => notice(e.message, true));
    return;
  }
  const title = withRoads
    ? c.jobs.length ? "生成道路矩阵并重新规划" : "生成道路矩阵并规划"
    : continuing
    ? "继续优化"
    : c.op
      ? "实时重排"
      : "仅按现有矩阵规划";
  const note = withRoads
    ? "按场景范围准备路网和道路矩阵，再自动规划并生成道路路径。缺失部分使用标明来源的几何估算；当前历史方案保持原样。"
    : continuing
    ? `沿用任务 #${source.id} 的冻结问题和最终结果作为搜索起点，另存一次求解记录。`
    : c.op
      ? "基于当前车辆位置、已完成与服务中事实、实际载荷及承诺锁，重新安排剩余工单。行驶中的下一站可被建议改道。"
      : "仅使用已保存的完整矩阵生成规划候选，不准备路网或生成道路路径。";
  const oldOptions = continuing
    ? c.cache.get(source.id)?.input.options
    : c.op?.policy?.options || c.scene.request.options;
  modal(
    title,
    `<p class="form-note">${esc(note)}</p>${c.dirty ? '<p class="banner warning">会先保存当前草稿，再提交求解。</p>' : ""}` +
      solveFields(oldOptions, state.health.totalThreads || 1),
    async (f) => {
      if (c.dirty) await saveDraft(c);
      if (withRoads) {
        await prepareGeoPlan(c, readOptions(f));
        return;
      }
      if (!continuing && !c.op && !matrixReady(c.scene.request))
        throw Error('在途矩阵未就绪，请使用“生成道路矩阵并规划”先完成矩阵准备。');
      let j;
      if (continuing)
        j = await api(`/v1/jobs/${source.id}/continue`, {
          options: readOptions(f),
        });
      else if (c.op)
        j = await api(path(c, "replan"), {
          scenarioId: c.id,
          expectedRevision: c.op.revision,
          options: readOptions(f),
        });
      else
        j = await api("/v1/jobs", {
          scenario: { scenarioId: c.id, version: c.scene.scenario.version },
          options: readOptions(f),
          kind: c.jobs.length ? 2 : 0,
          idempotencyKey: crypto.randomUUID(),
        });
      c.selectedJob = j.id;
      c.jobs.unshift(j);
      if (continuing && !c.op) {
        c.baselineJob = source.id;
        c.compare = true;
      }
      if (c.op) {
        c.liveJob = j.id;
        c.compare = true;
        c.tab = "operations";
      } else c.tab = "plans";
      routeHash(c);
    },
    { context: c, submit: c.dirty ? "保存并提交" : title, message: withRoads ? "路网规划已提交，完成后会通知。" : "操作已完成" },
  );
}
const locationIDs = (c) =>
  list(c.scene.request.problem.locations).map((l) => [
    l.id,
    label(c.scene.scenario.display, "locations", l.id),
  ]);
const agentIDs = (c) =>
  list(c.scene.request.problem.agents).map((a) => [
    a.id,
    label(c.scene.scenario.display, "agents", a.id),
  ]);
function startDialog(c, withoutPlan = false) {
  const j = withoutPlan ? null : currentJob(c);
  if (!withoutPlan && !sourceFresh(c, j))
    throw Error("请选择当前已保存场景的可用结果");
  if (c.dirty) throw Error("请先保存或还原场景草稿");
  const start = Math.min(
    ...list(c.scene.request.problem.agents).map((a) => num(a.startTime)),
  );
  if (!Number.isFinite(start)) throw Error("请先配置至少一个执行执行单元");
  modal(
    withoutPlan ? "接入执行状态" : "采用方案并进入执行",
    `<p class="form-note">${j ? "采用候选 #" + esc(j.id) + "，建立第一版生效计划。" : "建立执行状态，之后根据现场事实实时重排。"}实际出发、装货、服务完成需要逐项回报。</p>` +
      grid(
        select(
          "执行时钟",
          "clock",
          [
            [0, "实时：随系统时间推进"],
            [1, "演示 / 历史回放：只随事件推进"],
          ],
          0,
        ) +
          field("执行起始时间", "at", stamp(start), "text", "required") +
          select("默认装货地点", "loading", locationIDs(c)) +
          field("操作人", "actor", "调度员", "text", "required") +
          check("事件发生后持续重排", "autoReplan", true) +
          check("自动采用校验通过的候选", "autoAdopt", false),
      ),
    async (f) => {
      c.op = await api("/v1/operations/start", {
        scenario: { scenarioId: c.id, version: c.scene.scenario.version },
        initialJobId: j?.id || "",
        at: parseTime(f.get("at")),
        actor: f.get("actor"),
        policy: {
          clock: Number(f.get("clock")),
          autoReplan: f.has("autoReplan"),
          autoAdopt: f.has("autoAdopt"),
          loadingLocationId: f.get("loading"),
          options: c.cache.get(j?.id)?.input.options || c.scene.request.options,
        },
      });
      c.liveJob = "";
      c.tab = "operations";
      routeHash(c);
    },
    { context: c, submit: withoutPlan ? "进入执行" : "采用并开始执行" },
  );
}
function configuration(c, values) {
  return {
    scenarioId: c.id,
    expectedRevision: c.op.revision,
    policy: c.op.policy,
    routeCommitments: list(c.op.routeCommitments),
    timeCommitments: list(c.op.timeCommitments),
    actor: "调度员",
    reason: "调度设置",
    ...values,
  };
}
function policyDialog(c) {
  const p = c.op.policy || {},
    revision = c.op.revision;
  modal(
    "持续重排与方案生效",
    `<p class="form-note">持续重排：新单、状态或位置回报后，自动提交一次剩余任务重排。采用方式：决定候选通过校验后是否需要人工确认。两项设置独立。</p>` +
      grid(
        check("开启事件触发的持续重排", "autoReplan", p.autoReplan) +
          select(
            "新方案采用方式",
            "adoption",
            [
              ["manual", "人工确认（默认）"],
              ["auto", "自动采用通过校验的候选"],
            ],
            p.autoAdopt ? "auto" : "manual",
          ) +
          field(
            "事件合并等待（毫秒）",
            "debounce",
            p.debounceMs || 250,
            "number",
            'min="1" max="5000" required',
          ) +
          field("操作人", "actor", "调度员", "text", "required"),
      ) +
      solveFields(p.options, state.health.totalThreads || 1),
    async (f) => {
      c.op = await api(
        path(c, "configure"),
        configuration(c, {
          expectedRevision: revision,
          actor: f.get("actor"),
          reason: "调整持续重排与采用策略",
          policy: {
            ...p,
            autoReplan: f.has("autoReplan"),
            autoAdopt: f.get("adoption") === "auto",
            debounceMs: Number(f.get("debounce")),
            options: readOptions(f),
          },
        }),
      );
    },
    { context: c, submit: "保存策略" },
  );
}
function adoptDialog(c) {
  const j = currentJob(c);
  if (!sourceFresh(c, j)) throw Error("候选输入已过时，请先实时重排");
  const revision = c.op.revision,
    planVersion = currentPlanVersion(c),
    v = viewFor(c, j),
    count = changes(v.baseline, v.routes).length;
  modal(
    "采用候选方案",
    `<p class="form-note">将候选 #${esc(j.id)} 作为新的生效计划；${count} 张工单的执行单元、顺序或开始时间变化。服务器会再次检查实际执行、载荷、容量、承诺锁和路径可达性。</p>` +
      grid(
        field("确认人", "actor", "调度员", "text", "required") +
          field("采用说明", "reason", "调度确认", "text", "required"),
      ) +
      check("接受业务 Hard 罚分（无法绕过物理或承诺限制）", "override", false),
    async (f) => {
      c.op = await api(path(c, "adopt"), {
        scenarioId: c.id,
        expectedRevision: revision,
        expectedPlanVersion: planVersion,
        jobId: j.id,
        actor: f.get("actor"),
        reason: f.get("reason"),
        acceptBusinessViolations: f.has("override"),
      });
      c.liveJob = "";
      c.compare = false;
    },
    { context: c, submit: "校验并采用" },
  );
}
function eventFields(c) {
  return (
    field(
      "事件发生时间",
      "at",
      stamp(Math.ceil(nowAt(c.op))),
      "text",
      "required",
    ) + field("操作人", "actor", "调度员", "text", "required")
  );
}
async function executionEvent(c, revision, f, updates) {
  c.op = await api(path(c, "events"), {
    scenarioId: c.id,
    expectedRevision: revision,
    eventId: crypto.randomUUID(),
    at: parseTime(f.get("at")),
    actor: f.get("actor"),
    ...updates,
  });
}
function ticketDialog(c, id) {
  const t = list(c.op?.tickets).find((t) => t.ticketId === id);
  if (!t) throw Error("该工单不在当前执行事实中");
  const phase = tp(t),
    revision = c.op.revision,
    transitions = [
      ["EN_ROUTE", "出发 / 改道"],
      ["ARRIVED", "到达"],
      ["SERVING", "开始服务 / 更新剩余时长"],
      ["COMPLETED", "完成服务"],
      ["CANCELLED", "取消"],
      ["EXCEPTION", "异常 / 中止服务"],
      ["PENDING", "恢复待执行"],
    ],
    allowed = [
      [0, 4, 5],
      [1, 0, 4, 5],
      [2, 0, 4, 5],
      [3, 2, 5],
      [],
      [],
      [6, 4],
    ][phase];
  if (!allowed.length) throw Error("已完成或取消的工单不可改写");
  const agent =
    t.agentId ||
    assignments(planRoutes(c.scene.request, c.op.plan)).get(id)?.agentId;
  modal(
    label(c.scene.scenario.display, "tickets", id),
    `<p class="form-note">当前 ${phaseNames[phase]}。此操作登记实际执行情况，会使旧候选过时；${c.op.policy?.autoReplan ? "系统随后自动重排。" : "可随后手动点击实时重排。"}</p>` +
      grid(
        select(
          "执行进展",
          "phase",
          allowed.map((i) => transitions[i]),
        ) +
          select("执行执行单元", "agent", agentIDs(c), agent) +
          eventFields(c) +
          field(
            "剩余服务分钟（选填）",
            "remaining",
            "",
            "number",
            'min="0" step="0.1"',
          ) +
          field("备注 / 异常原因", "note", t.note || ""),
      ),
    (f) =>
      executionEvent(c, revision, f, {
        tickets: [
          {
            ticketId: id,
            phase: "TICKET_PHASE_" + f.get("phase"),
            agentId: f.get("agent"),
            note: f.get("note"),
            ...(f.get("remaining") === ""
              ? {}
              : {
                  remainingServiceSeconds: String(
                    Math.round(Number(f.get("remaining")) * 60),
                  ),
                }),
          },
        ],
      }),
    { context: c, submit: "记录执行事实" },
  );
}
function vehicleDialog(c, id) {
  const v = c.op.vehicles.find((v) => v.agentId === id),
    revision = c.op.revision;
  modal(
    "位置与执行单元状态 · " + label(c.scene.scenario.display, "agents", id),
    grid(
      select(
        "当前状态",
        "phase",
        vehicleNames.map((t, i) => [i, t]),
        vp(v),
      ) +
        field(
          "经度",
          "lng",
          v.position?.lng,
          "number",
          'step="any" min="-180" max="180" required',
        ) +
        field(
          "纬度",
          "lat",
          v.position?.lat,
          "number",
          'step="any" min="-90" max="90" required',
        ) +
        field(
          "累计行驶分钟",
          "travel",
          num(v.travelSeconds) / 60,
          "number",
          'min="0" step="0.01" required',
        ) +
        field("最早可用时间（选填）", "available", "") +
        eventFields(c),
    ),
    (f) =>
      executionEvent(c, revision, f, {
        vehicles: [
          {
            agentId: id,
            phase: Number(f.get("phase")),
            position: {
              id: `observed-${id}`,
              lng: Number(f.get("lng")),
              lat: Number(f.get("lat")),
            },
            travelSeconds: String(Math.round(Number(f.get("travel")) * 60)),
            ...(f.get("available")
              ? { availableAt: parseTime(f.get("available")) }
              : {}),
          },
        ],
      }),
    { context: c, submit: "记录位置与状态" },
  );
}
function cargoDialog(c) {
  const cargo = list(c.op.cargo).filter((x) => !x.delivered),
    revision = c.op.revision;
  if (!cargo.length) throw Error("当前没有需要装卸或交接的货物");
  modal(
    "装卸与实物交接",
    grid(
      select(
        "工单货物",
        "ticket",
        cargo.map((x) => [
          x.ticketId,
          `${label(c.scene.scenario.display, "tickets", x.ticketId)} · ${x.agentId ? "车载 " + x.agentId : "存放 " + x.locationId}`,
        ]),
      ) +
        select("交给执行单元", "to", [["", "卸至地点"], ...agentIDs(c)]) +
        select("实际交接地点", "location", locationIDs(c)) +
        eventFields(c),
    ) +
      '<p class="hint">按完整工单货物交接。双方执行单元必须已停靠在同一地点，需先回报实际位置；这里不会自动移动车辆。</p>',
    (f) => {
      const item = cargo.find((x) => x.ticketId === f.get("ticket"));
      return executionEvent(c, revision, f, {
        cargo: [
          {
            ticketId: item.ticketId,
            fromAgentId: item.agentId || "",
            toAgentId: f.get("to"),
            locationId: f.get("location"),
          },
        ],
      });
    },
    { context: c, submit: "确认实物交接" },
  );
}
function lockDialog(c, id) {
  id = id || c.selectedAgent || c.op.vehicles[0]?.agentId;
  const route = list(c.op.routeCommitments).find((x) => x.agentId === id),
    revision = c.op.revision,
    times = list(c.op.timeCommitments).filter((x) => x.agentId === id);
  modal(
    "承诺锁定 · " + label(c.scene.scenario.display, "agents", id),
    `<p class="form-note">已在服务和已完成工单由执行事实保护。这里锁定尚未执行的路线前缀或单个预约时间；正在前往的下一站默认允许建议改道。</p>` +
      grid(
        field(
          "接下来固定顺序（工单编号，逗号分隔）",
          "prefix",
          list(route?.prefix).join(","),
        ) +
          select("新增 / 修改预约承诺", "ticket", [
            ["", "不修改现有预约"],
            ...list(c.scene.request.problem.tickets)
              .filter(
                (t) =>
                  ![3, 4, 5].includes(
                    tp(c.op.tickets.find((x) => x.ticketId === t.id)),
                  ),
              )
              .map((t) => [
                t.id,
                label(c.scene.scenario.display, "tickets", t.id),
              ]),
          ]) +
          field("承诺最早开始（选填）", "earliest", "") +
          field("承诺最晚开始（选填）", "latest", "") +
          field("操作人", "actor", "调度员", "text", "required") +
          field("原因", "reason", "", "text", "required"),
      ) +
      check("解除该执行单元的全部预约时间承诺", "clearTimes", false) +
      `<p class="hint">现有预约：${times.map((t) => `${esc(t.ticketId)} ${t.earliest === undefined ? "不限" : stamp(t.earliest)}—${t.latest === undefined ? "不限" : stamp(t.latest)}`).join("；") || "无"}。固定顺序留空会解除该执行单元的路线前缀锁。</p>`,
    async (f) => {
      const prefix = csv(f.get("prefix"));
      let routeCommitments = list(c.op.routeCommitments).filter(
          (x) => x.agentId !== id,
        ),
        timeCommitments = list(c.op.timeCommitments).filter(
          (x) =>
            !(f.has("clearTimes") && x.agentId === id) &&
            x.ticketId !== f.get("ticket"),
        );
      if (prefix.length) routeCommitments.push({ agentId: id, prefix });
      if (f.get("ticket"))
        timeCommitments.push({
          ticketId: f.get("ticket"),
          agentId: id,
          ...(f.get("earliest")
            ? { earliest: parseTime(f.get("earliest")) }
            : {}),
          ...(f.get("latest") ? { latest: parseTime(f.get("latest")) } : {}),
        });
      c.op = await api(
        path(c, "configure"),
        configuration(c, {
          expectedRevision: revision,
          routeCommitments,
          timeCommitments,
          actor: f.get("actor"),
          reason: f.get("reason"),
        }),
      );
    },
    { context: c, submit: "保存承诺" },
  );
}
function adjustDialog(c, ticketId) {
  const j = currentJob(c);
  if (!sourceFresh(c, j))
    throw Error("请先实时重排生成当前事实下的候选，再调整安排");
  const cached = c.cache.get(j.id),
    p = cached.input.problem,
    solution = clone(cached.result.solution),
    ticketIndex = p.tickets.findIndex((t) => t.id === ticketId);
  if (ticketIndex < 0) throw Error("该工单已不在可调整的候选中");
  const route = list(solution.routes).find((r) =>
    list(r.tickets).includes(ticketIndex),
  );
  modal(
    "调整安排 · " + label(c.scene.scenario.display, "tickets", ticketId),
    '<p class="form-note">修改归属或访问顺序后，保存为独立的人工候选并重新评分。现有计划不变，候选需要再采用。</p>' +
      grid(
        select(
          "目标执行单元",
          "agent",
          [
            ["", "设为未分配"],
            ...list(p.agents).map((a, i) => [
              i,
              label(c.scene.scenario.display, "agents", a.id),
            ]),
          ],
          route?.agent ?? (route ? 0 : ""),
        ) +
          select("插入位置", "before", [
            ["", "目标路线最后"],
            ...list(p.tickets)
              .map((t, i) => [
                i,
                t.id === ticketId
                  ? ""
                  : label(c.scene.scenario.display, "tickets", t.id),
              ])
              .filter(
                ([i, name]) => name && !p.tickets[i].id.startsWith("@vrp/"),
              ),
          ]) +
          field("操作人", "actor", "调度员", "text", "required") +
          field("调整原因", "reason", "", "text", "required"),
      ),
    async (f) => {
      const moving = new Set([ticketIndex]);
      const rule = list(p.execution?.tickets).find(
        (t) => (t.ticket || 0) === ticketIndex,
      );
      if (rule?.loadBefore !== undefined) moving.add(rule.loadBefore);
      solution.routes ||= [];
      for (const r of solution.routes)
        r.tickets = list(r.tickets).filter((i) => !moving.has(i));
      if (f.get("agent") !== "") {
        const agent = Number(f.get("agent"));
        let target = solution.routes.find((r) => (r.agent || 0) === agent);
        if (!target) {
          target = { agent, tickets: [] };
          solution.routes.push(target);
        }
        target.tickets ||= [];
        let position = target.tickets.length;
        if (f.get("before") !== "") {
          position = target.tickets.indexOf(Number(f.get("before")));
          if (position < 0) throw Error("插入位置必须是目标执行单元路线上的工单");
        }
        target.tickets.splice(
          position,
          0,
          ...[...moving].filter((i) => i !== ticketIndex),
          ticketIndex,
        );
      }
      const job = await api(`/v1/jobs/${j.id}/adjust`, {
        solution,
        actor: f.get("actor"),
        reason: f.get("reason"),
      });
      c.jobs.unshift(job);
      c.selectedJob = job.id;
      if (c.op) c.liveJob = job.id;
      else c.baselineJob = j.id;
      c.compare = true;
      await loadResult(c, job);
    },
    { context: c, submit: "校验并生成候选" },
  );
}
async function changeTab(c, tab) {
  rememberView(c);
  c.tab = tab;
  routeHash(c);
  if (tab === "history") {
    await loadHistory(c);
    if (!c.historySnapshot && c.historyEntries[0])
      await selectHistory(c, c.historyEntries[0].revision);
  }
  render();
}
async function perform(name, el) {
  const c = ctx();
  switch (name) {
    case 'delete-plan':
      await deletePlanDialog(c, el.dataset.job || c?.selectedJob);
      break;
    case 'geo-plan':
      solveDialog(c);
      break;
    case 'solve-existing':
      solveDialog(c, false, true);
      break;
    case 'open-generic-task':
      await openGenericTask(el.dataset.task);
      break;
    case 'close-generic-task':
      state.taskDetail = null;
      routeHash();
      render();
      break;
    case 'cancel-generic-task':
      await run(async () => {
        await api(`/v1/tasks/${encodeURIComponent(el.dataset.task)}/cancel`, {});
        if (state.view === 'tasks') await loadGlobalJobs();
      }, '已请求停止，已经完成的产物保留。');
      break;
    case 'retry-generic-task':
      await run(async () => {
        const task = await api(`/v1/tasks/${encodeURIComponent(el.dataset.task)}/retry`, {});
        await openGenericTask(task.id);
      }, '已创建重试任务。');
      break;
    case 'download-task-artifact':
      download(await api(el.dataset.task.startsWith('task-')
        ? `/v1/tasks/${encodeURIComponent(el.dataset.task)}/artifacts/${encodeURIComponent(el.dataset.artifact)}`
        : `/v1/jobs/${encodeURIComponent(el.dataset.task)}/result`, undefined, 'application/octet-stream'), el.dataset.name || el.dataset.artifact);
      break;
    case 'road-catalog': {
      const catalog = await api('/v1/road-packages/catalog');
      state.roadCatalog = list(catalog.entries);
      if (!state.roadCatalog.length) throw Error(catalog.message || '暂无可用下载目录，请稍后重试或导入本地 PBF。');
      modal('下载路网区域包', select('区域', 'region', state.roadCatalog.map(p => [p.id,p.name + (p.cached ? ' · 已有缓存' : '')]), '') + '<p class="hint">按区域下载标准 PBF。场景自动准备会根据实际覆盖与新增下载量选择组合。</p>', async f => {
        const task = await api('/v1/road-packages/prepare', {regionId:f.get('region')});
        await openGenericTask(task.id);
      }, {context:null,submit:'开始准备',reload:false});
      break;
    }
    case 'road-import':
      modal('导入本地 PBF', grid(field('服务器可见的 PBF 文件路径', 'path', '', 'text', 'required placeholder="C:/maps/region.osm.pbf"') + field('显示名称（选填）', 'name', '')) + '<p class="hint">此路径位于运行 vrp-server 的机器。文件经过校验后登记为可复用源包，原始文件保持不变。</p>', async f => {
        const task = await api('/v1/road-packages/import', {path:f.get('path'),name:f.get('name')});
        await openGenericTask(task.id);
      }, {context:null,submit:'校验并导入',reload:false});
      break;
    case 'package-tab':
      state.packageTab=el.dataset.packageTab;
      await loadRoadPackages();routeHash();render();
      break;
    case 'basemap-import':
      modal('导入离线底图',grid(field('服务器可见的 PMTiles 文件或目录','path','','text','required placeholder="C:/maps/beijing.pmtiles"')+field('显示名称（选填）','name',''))+'<p class="hint">支持 PMTiles v3：Protomaps v4 兼容矢量包或 PNG / JPEG / WebP 栅格包。目录中须有 archive.pmtiles。导入只使用本地文件。</p>',async f=>{
        const task=await api('/v1/basemap-packages/import',{path:f.get('path'),name:f.get('name')});await openGenericTask(task.id);
      },{context:null,submit:'验证并导入',reload:false});
      break;
    case 'basemap-prepare':
      await prepareBasemapDialog({level:Number(el.dataset.level || 14)});
      break;
    case 'basemap-detail':
      state.packageTab='basemaps';state.view='roads';state.basemapDetail=await api(`/v1/basemap-packages/${encodeURIComponent(el.dataset.package)}`);
      await loadRoadPackages();routeHash();render();
      break;
    case 'basemap-close':
      state.basemapDetail=null;routeHash();render();break;
    case 'basemap-enable':
      await run(async()=>{
        const p=state.basemapPackages.find(p=>p.id===el.dataset.package);
        await api(`/v1/basemap-packages/${encodeURIComponent(p.id)}/enable`,{enabled:!p.enabled,expectedVersion:p.version});
        invalidateBasemaps();await loadRoadPackages();
      },'底图包启用设置已更新。');break;
    case 'basemap-update': {
      const p=state.basemapPackages.find(p=>p.id===el.dataset.package);
      await prepareBasemapDialog({bounds:p.bounds,level:Number(p.nativeMaxZoom),updateId:p.id});break;
    }
    case 'basemap-delete': {
      const p=state.basemapPackages.find(p=>p.id===el.dataset.package);
      modal('清理未引用底图包',`<p>清理 ${esc(p.name)}。服务器将再次检查场景、历史视图和活动任务引用。</p>`,async()=>{
        await api(`/v1/basemap-packages/${encodeURIComponent(p.id)}`,undefined,'application/json','DELETE');
        state.basemapDetail=null;invalidateBasemaps();await loadRoadPackages();
      },{context:null,submit:'清理',destructive:true,reload:false});break;
    }
    case 'road-detail':
      state.roadDetail = await api(`/v1/road-packages/${encodeURIComponent(el.dataset.package)}`);
      await loadRoadPackages();
      state.view = 'roads';
      routeHash();
      render();
      document.querySelector('.road-package-detail')?.scrollIntoView({block:'nearest'});
      break;
    case 'road-close':
      state.roadSerial = (state.roadSerial || 0) + 1;
      state.roadDetail = null;
      routeHash();
      render();
      break;
    case 'road-enable':
      await run(async () => {
        const p = state.roadPackages.find(p => p.id === el.dataset.package);
        await api(`/v1/road-packages/${encodeURIComponent(el.dataset.package)}/enable`, {enabled:el.dataset.enabled === 'true',expectedVersion:p?.version});
        await loadRoadPackages();
      }, '启用设置已更新；已有任务仍使用原版本。');
      break;
    case 'road-update':
    case 'road-rebuild':
    case 'road-force-download':
      await run(async () => {
        const task = await api(`/v1/road-packages/${encodeURIComponent(el.dataset.package)}/${name === 'road-rebuild' ? 'rebuild' : 'update'}`, {forceDownload:name === 'road-force-download'});
        await openGenericTask(task.id);
      }, '已提交路网准备任务。');
      break;
    case 'road-build':
      if (!state.scenes.length) throw Error('先保存一个包含地点经纬度的场景，再按该场景范围构图。');
      modal('按场景范围构图', `<label>需求场景<select name="scene" required>${state.scenes.map(scene => `<option value="${esc(scene.id)}">${esc(scene.name)}</option>`).join('')}</select></label><p class="hint">仅提取所选场景及绕行缓冲内的道路，生成可复用路由图和矩阵产物。</p>`, async form => {
        const scene = await api(`/v1/scenarios/${encodeURIComponent(form.get('scene'))}`);
        const task = await api('/v1/tasks', {idempotencyKey:crypto.randomUUID(),input:{matrix:{request:scene.request,roadPackageIds:[el.dataset.package]}}});
        await openGenericTask(task.id);
      }, {context:null,submit:'构图',reload:false});
      break;
    case 'road-delete': {
      const p = state.roadPackages.find(p => p.id === el.dataset.package) || state.roadDetail;
      if (!p) break;
      modal('清理未引用路网包', `<p>清理 ${esc(p.name || p.id)} 的本地源包及未引用缓存。服务器会再次检查场景、任务和路径引用。</p>`, async () => {
        await api(`/v1/road-packages/${encodeURIComponent(p.id)}`, undefined, 'application/json', 'DELETE');
        if (state.roadDetail?.id === p.id) state.roadDetail = null;
        await loadRoadPackages();
      }, {context:null,submit:'清理',destructive:true,reload:false});
      break;
    }
    case "task-info": {
      const id = el.dataset.job, job = c.jobs.find((job) => job.id === id) || await api(`/v1/jobs/${encodeURIComponent(id)}`);
      if (state.sceneId !== c.id || state.view !== "scene") return;
      if (!c.jobs.some((item) => item.id === job.id)) c.jobs.push(job);
      modal(`任务信息 · #${job.id}`, jobDetails(job, true), () => {}, { context: c, readOnly: true, submit: "关闭" });
      break;
    }
    case "show-unassigned": {
      selectResultTab(c, "unassigned");
      render();
      $("result-tab-unassigned")?.focus({ preventScroll: true });
      break;
    }
    case "delete-scene":
      await deleteSceneDialog(el?.dataset.scenarioId ? state.scenes.find((s) => s.id === el.dataset.scenarioId) : c?.scene?.scenario);
      break;
    case "refresh":
      await refresh();
      break;
    case "new-scene":
      newScene();
      break;
    case "import-scene":
      newScene(true);
      break;
    case "connect":
      modal(
        "访问设置",
        '<p class="form-note">远程访问请输入服务配置的 Bearer 令牌。令牌仅保存在当前页面内存中。</p><div class="form-stack">' +
          field("访问令牌", "token", "", "password", 'autocomplete="off"') +
          "</div>",
        async (f) => {
          state.token = f.get("token");
          state.health = await api("/healthz");
          await loadScenes();
          if (state.sceneId) await openScene(state.sceneId);
          else {
            state.view = "scenes";
            render();
          }
        },
        { context: null, submit: "连接服务" },
      );
      break;
    case "matrix-prev":
      c.matrixPage = Math.max(0, (c.matrixPage || 0) - 1);
      render();
      break;
    case "matrix-next":
      c.matrixPage = (c.matrixPage || 0) + 1;
      render();
      break;
    case "geometry-import": {
      const j = (el.dataset.job ? c.jobs.find((job) => job.id === el.dataset.job) : currentJob(c)) || c.jobs.find((j) => j.id === c.op?.plan?.jobId);
      if (!j?.hasResult) break;
      if ($("dialog").open) $("dialog").close();
      modal(
        "导入道路路径",
        '<label>原生 JobGeometry JSON<input name="file" type="file" accept=".json" required></label><p class="hint">路径必须对应当前任务的车辆与访问顺序。导入只更新展示，不改变排程。</p>',
        async (f) => {
          const value = JSON.parse(await f.get("file").text());
          if (value.jobId && value.jobId !== j.id)
            throw new Error("路径属于另一个任务");
          await api(`/v1/jobs/${j.id}/geometry`, { ...value, jobId: j.id });
          invalidateGeometry(j.id);
        },
        { context: c, submit: "导入路径" },
      );
      break;
    }
    case "save-draft":
      await run(() => saveDraft(c), "场景已保存，可重新规划或实时重排");
      break;
    case "export-scene":
      beginDraft(c);
      download(
        new Blob(
          [
            JSON.stringify(
              {
                ...c.draft,
                request: c.rawDirty ? JSON.parse(c.rawDraft) : c.draft.request,
              },
              null,
              2,
            ),
          ],
          { type: "application/json" },
        ),
        `${c.id}.json`,
      );
      break;
    case "discard-draft":
      modal(
        "还原当前场景草稿",
        '<p class="form-note">将本场景草稿还原为服务端最新保存的业务版本。其他场景的草稿不受影响。</p>',
        () => {
          c.draft = null;
          c.dirty = false;
          c.rawDirty = false;
          resetSheetHistory(c);
          delete c.rawDraft;
          beginDraft(c);
        },
        { context: c, submit: "还原" },
      );
      break;
    case "solve":
      solveDialog(c);
      break;
    case "continue":
      solveDialog(c, true);
      break;
    case "start":
      startDialog(c);
      break;
    case "start-empty":
      startDialog(c, true);
      break;
    case "policy":
      policyDialog(c);
      break;
    case "adopt":
      adoptDialog(c);
      break;
    case "new-ticket":
      editObject(c, "tickets", undefined, true);
      break;
    case "edit-object":
      editObject(
        c,
        el.dataset.kind,
        el.dataset.index === undefined ? undefined : Number(el.dataset.index),
      );
      break;
    case "linked-location":
      editLinkedLocation(c, el.dataset.kind, Number(el.dataset.index), el.dataset.field);
      break;
    case "close-location":
      delete c.dataLocation;
      render();
      break;
    case "delete-object":
      modal(
        "移除草稿中的对象",
        '<p class="form-note">移除只改变本场景草稿，保存后才形成新的业务版本。</p>',
        () => { checkpoint(c); removeObject(c, el.dataset.kind, Number(el.dataset.index)); },
        { context: c, submit: "移除" },
      );
      break;
    case "scene-properties":
      sceneProperties(c);
      break;
    case "scene-date":
      sceneDate(c);
      break;
    case "sheet-help":
      modal("工作表操作", `<div class="sheet-help-content"><p>${esc(el.dataset.help)}</p><dl><dt>Tab / Enter</dt><dd>切换单元格</dd><dt>Shift + 点击</dt><dd>选择区域</dd><dt>Ctrl / ⌘ + C / V</dt><dd>复制 / 粘贴 Excel 表格</dd><dt>Ctrl / ⌘ + Z / Y</dt><dd>撤销 / 重做</dd><dt>Ctrl / ⌘ + D</dt><dd>向下填充</dd><dt>Ctrl / ⌘ + S</dt><dd>保存场景草稿</dd><dt>Esc</dt><dd>取消当前单元格的无效输入</dd></dl></div>`, () => {},
        { context: c, submit: "关闭", compact: true, readOnly: true });
      break;
    case "apply-json": {
      const request = JSON.parse($("json-editor").value);
      if (!request.problem) throw Error("需要包含 problem 的 SolveRequest");
      checkpoint(c);
      c.draft.request = request;
      c.dirty = true;
      c.rawDirty = false;
      c.rawDraft = $("json-editor").value;
      render();
      notice("已应用到草稿，保存时进行完整校验");
      break;
    }
    case "data-prev":
      c.dataPage = Math.max(0, c.dataPage - 1);
      render();
      break;
    case "data-next":
      c.dataPage++;
      render();
      break;
    case "go-data":
      await changeTab(c, "data");
      break;
    case "go-plans":
      await changeTab(c, "plans");
      break;
    case "show-live":
      c.liveJob =
        c.tab === "plans" &&
        currentJob(c)?.executionBasis &&
        currentJob(c)?.id !== c.op?.plan?.jobId
          ? c.selectedJob
          : "";
      c.compare = !!c.liveJob;
      await changeTab(c, "operations");
      break;
    case "show-candidate": {
      const j = latestJob(c);
      c.liveJob = j.id;
      c.compare = true;
      if (j.hasResult) await loadResult(c, j);
      render();
      break;
    }
    case "show-published":
      c.liveJob = "";
      c.compare = false;
      render();
      break;
    case "zoom-out":
      c.zoom = Math.max(0.5, (c.zoom || 1) - 0.25);
      render();
      break;
    case "zoom-in":
      c.zoom = Math.min(6, (c.zoom || 1) + 0.25);
      render();
      break;
    case "day-range":
      c.fullDay = !c.fullDay;
      render();
      break;
    case "schedule-density":
      c.scheduleDensity = el.dataset.density;
      render();
      break;
    case "toggle-inspector":
      c.inspectorClosed = !c.inspectorClosed;
      render();
      break;
    case "clear-selection":
      c.mapOverview = true;
      c.selectedTicket = "";
      c.selectedAgent = "";
      c.resourceFilter = "";
      c.filter = "";
      render();
      break;
    case "ticket-progress":
      ticketDialog(c, el.dataset.ticket);
      break;
    case "vehicle-progress":
      vehicleDialog(c, el.dataset.agent);
      break;
    case "lock":
      lockDialog(c, el.dataset.agent);
      break;
    case "cargo":
      cargoDialog(c);
      break;
    case "adjust":
      adjustDialog(c, el.dataset.ticket);
      break;
    case "ack": {
      const revision = c.op.revision,
        version = c.op.plan.version,
        id = el.dataset.agent;
      modal(
        "人工登记执行单元确认",
        `<p class="form-note">确认已收到 ${esc(label(c.scene.scenario.display, "agents", id))} 对当前计划 v${esc(version)} 的实际确认。这是人工登记，不会发送车辆通知。</p>`,
        async () => {
          c.op = await api(path(c, "ack"), {
            scenarioId: c.id,
            expectedRevision: revision,
            planVersion: version,
            agentId: id,
          });
        },
        { context: c, submit: "登记已确认" },
      );
      break;
    }
    case "close-operation": {
      const revision = c.op.revision;
      modal(
        "结束本场景执行",
        '<p class="form-note">结束后保留全部历史，释放物理执行单元占用。本场景的执行变为只读；在途、服务中的工单或车载货物会阻止结束。</p>' +
          grid(field("操作人", "actor", "调度员", "text", "required")),
        async (f) => {
          c.op = await api(path(c, "close"), {
            scenarioId: c.id,
            expectedRevision: revision,
            actor: f.get("actor"),
          });
        },
        { context: c, submit: "校验并结束" },
      );
      break;
    }
    case "cancel-job":
      await run(async () => {
        await api(`/v1/jobs/${el.dataset.job}/cancel`, {});
        if (state.view === "tasks") await loadGlobalJobs();
      }, "已请求停止，正在运行的任务会保留已找到的结果");
      break;
    case "download-input":
    case "download-result": {
      const job = el.dataset.job || currentJob(c)?.id;
      if (!job) throw Error("没有选中任务");
      const type = name === "download-input" ? "request" : "result";
      download(
        await api(`/v1/jobs/${job}/${type}`, undefined, "application/protobuf"),
        `job-${job}-${type}.pb`,
      );
      break;
    }
    case "history-refresh":
      await loadHistory(c);
      render();
      break;
    case "history-more":
      await loadHistory(c, true);
      render();
      break;
    case "more-global-jobs":
      await loadGlobalJobs(true);
      render();
      break;
  }
}
async function safely(fn) {
  try {
    await fn();
  } catch (e) {
    notice(e.message, true);
  }
}
document.addEventListener("click", (event) => {
  document.querySelectorAll('.scene-more[open]').forEach(menu => {
    if (!menu.contains(event.target) || event.target.closest('button')) menu.open = false;
  });
  const el = event.target.closest(
    "button,[data-select-ticket],[data-select-agent]",
  );
  if (!el || el.disabled || state.busy) return;
  safely(async () => {
    if (el.dataset.nav) {
      rememberView(ctx());
      state.view = el.dataset.nav;
      routeHash();
      if (state.view === "tasks") await loadGlobalJobs();
      if (state.view === "roads") await loadRoadPackages();
      render();
    } else if (el.dataset.scene) await openScene(el.dataset.scene);
    else if (el.dataset.tab) await changeTab(ctx(), el.dataset.tab);
    else if (el.dataset.resultTab) {
      selectResultTab(ctx(), el.dataset.resultTab);
      render();
    }
    else if (el.dataset.dataTab) {
      const c = ctx();
      syncRawDraft(c);
      rememberView(c);
      c.dataTab = el.dataset.dataTab;
      c.dataPage = 0;
      render();
    } else if (el.dataset.openJob)
      await openScene(el.dataset.jobScene, "plans", el.dataset.openJob);
    else if (el.dataset.history) {
      await selectHistory(ctx(), el.dataset.history);
      render();
    } else if (el.dataset.selectTicket) {
      const c = ctx();
      c.mapOverview = false;
      c.selectedTicket = el.dataset.selectTicket;
      const view = viewFor(c, currentJob(c));
      c.selectedAgent =
        el.dataset.agent ||
        assignments(view.routes).get(c.selectedTicket)?.agentId ||
        "";
      if (c.resourceFilter && c.selectedAgent && c.resourceFilter !== c.selectedAgent)
        c.resourceFilter = c.selectedAgent;
      selectResultTab(c, "schedule");
      c.inspectorClosed = false;
      render();
    } else if (el.dataset.selectAgent) {
      ctx().mapOverview = false;
      ctx().selectedAgent = el.dataset.selectAgent;
      ctx().selectedTicket = "";
      ctx().inspectorClosed = false;
      render();
    } else if (el.dataset.action) await perform(el.dataset.action, el);
  });
});
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    const menu = document.querySelector('.scene-more[open]');
    if (menu) { e.preventDefault(); menu.open = false; menu.querySelector('summary').focus({ preventScroll: true }); return; }
  }
  const resultTab = e.target.closest('[role="tab"][data-result-tab]');
  if (resultTab && ["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) {
    e.preventDefault();
    if (state.busy) return;
    const tabs = [...resultTab.closest('[role="tablist"]').querySelectorAll('[data-result-tab]')], index = tabs.indexOf(resultTab);
    const next = e.key === "Home" ? 0 : e.key === "End" ? tabs.length - 1 : (index + (e.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
    selectResultTab(ctx(), tabs[next].dataset.resultTab);
    render();
    document.querySelector('.result-tabs [aria-selected="true"]')?.focus({ preventScroll: true });
    return;
  }
  const mainTab = e.target.closest('[role="tab"][data-tab]');
  if (mainTab && ["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) {
    e.preventDefault();
    if (state.busy) return;
    const tabs = [...mainTab.closest('[role="tablist"]').querySelectorAll('[data-tab]')], index = tabs.indexOf(mainTab);
    const next = e.key === "Home" ? 0 : e.key === "End" ? tabs.length - 1
      : (index + (e.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
    safely(async () => { await changeTab(ctx(), tabs[next].dataset.tab); tabs[next].focus({ preventScroll: true }); });
    return;
  }
  const dataTab = e.target.closest('[role="tab"][data-data-tab]');
  if (dataTab && ["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) {
    e.preventDefault();
    if (state.busy) return;
    const tabs = [...dataTab.closest(".data-tabs").querySelectorAll("[data-data-tab]")];
    const index = tabs.indexOf(dataTab);
    const next = e.key === "Home" ? 0 : e.key === "End" ? tabs.length - 1
      : (index + (e.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
    tabs[next].click();
    document.querySelector('.data-tabs [aria-selected="true"]')?.focus({ preventScroll: true });
    return;
  }
  if (
    (e.key === "Enter" || e.key === " ") &&
    e.target.matches("g[data-select-ticket]")
  ) {
    e.preventDefault();
    e.target.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  }
});
document.addEventListener("change", (event) =>
  safely(async () => {
    const c = ctx(),
      el = event.target;
    switch (el.id) {
      case "progress-score":
        c.scoreMode = el.value;
        render();
        break;
      case "progress-range":
        c.trendRange = el.value;
        render();
        break;
      case "matrix-profile":
        c.matrixProfile = Number(el.value);
        c.matrixPage = 0;
        render();
        break;
      case "matrix-from":
        c.matrixFrom = el.value;
        c.matrixPage = 0;
        render();
        break;
      case "matrix-to":
        c.matrixTo = el.value;
        c.matrixPage = 0;
        render();
        break;
      case "selected-job":
        c.selectedJob = el.value;
        {
          const j = currentJob(c);
          if (j) await Promise.all([j.hasResult ? loadResult(c, j) : null, loadTrend(c, j)]);
        }
        c.compare = false;
        routeHash(c);
        render();
        break;
      case "live-job":
        c.liveJob = el.value;
        c.compare = !!el.value;
        {
          const j = currentJob(c);
          if (j) await Promise.all([j.hasResult ? loadResult(c, j) : null, loadTrend(c, j)]);
        }
        render();
        break;
      case "baseline-job":
        c.baselineJob = el.value;
        c.compare = !!el.value;
        if (el.value)
          await loadResult(
            c,
            c.jobs.find((j) => j.id === el.value),
          );
        render();
        break;
      case "resource-filter":
        c.mapOverview = !el.value;
        c.resourceFilter = el.value;
        c.selectedAgent = el.value;
        c.selectedTicket = "";
        render();
        break;
      case "compare":
        c.compare = el.checked;
        render();
        break;
      case "global-filter":
        state.globalFilter = el.value;
        render();
        break;
      case "global-scene":
        state.globalScene = el.value;
        await loadGlobalJobs();
        render();
        break;
      case 'global-kind':
        state.globalKind = el.value;
        await loadGlobalJobs();
        render();
        break;
    }
  }),
);
let inputTimer;
document.addEventListener("input", (event) => {
  const el = event.target,
    c = ctx();
  if (el.id === "scene-search") {
    state.sceneSearch = el.value;
    renderSidebar();
    if (state.view === "scenes") $("global-content").innerHTML = scenesPanel();
  } else if (el.id === 'road-search') {
    state.roadSearch = el.value;
    clearTimeout(inputTimer);
    inputTimer = setTimeout(() => { if (state.view === 'roads') render(); }, 160);
  } else if (el.id === "schedule-filter" || el.dataset.sheetFilter) {
    if (el.id === "schedule-filter") c.filter = el.value;
    else {
      c.sheetFilters ||= {};
      c.sheetFilters[el.dataset.sheetFilter] = el.value;
    }
    clearTimeout(inputTimer);
    inputTimer = setTimeout(() => {
      if (ctx() === c) render();
    }, 160);
  } else if (el.id === "json-editor") {
    c.rawDraft = el.value;
    c.rawDirty = true;
    c.dirty = true;
    renderDraftState(c);
  }
});
$("dialog-close").onclick = $("dialog-cancel").onclick = () => {
  if (!state.busy) $("dialog").close();
};
$("dialog").addEventListener("cancel", (e) => {
  if (state.busy) e.preventDefault();
});
$("notice-close").onclick = (event) => { event.stopPropagation(); notice(""); };
$("connect").onclick = () => safely(() => perform("connect"));
$("new-scene").onclick = () => newScene();
$("import-scene").onclick = () => newScene(true);
$("more-scenes").onclick = () =>
  safely(async () => {
    await loadScenes(true);
    if (state.view === "scenes") render();
  });
window.addEventListener("beforeunload", (event) => {
  if ([...state.contexts.values()].some((c) => c.dirty)) {
    event.preventDefault();
    event.returnValue = "";
  }
});
async function navigate() {
  const parts = location.hash.slice(1).split("/");
  if (parts[0] === "scene" && parts[1])
    await openScene(
      decodeURIComponent(parts[1]),
      ["data", "plans", "operations", "history"].includes(parts[2])
        ? parts[2]
        : undefined,
      parts[2] === "plans" && /^\d+$/.test(parts[3] || "") ? parts[3] : undefined,
    );
  else if (["scenes", "roads", "tasks", "settings"].includes(parts[0])) {
    state.view = parts[0];
    if (state.view === "tasks") {
      state.taskDetail = parts[1] ? await api(`/v1/tasks/${encodeURIComponent(decodeURIComponent(parts[1]))}`) : null;
      await loadGlobalJobs();
    }
    if (state.view === 'roads') {
      state.packageTab=parts[1]==='basemaps'?'basemaps':'roads';
      if(state.packageTab==='basemaps') state.basemapDetail=parts[2]?await api(`/v1/basemap-packages/${encodeURIComponent(decodeURIComponent(parts[2]))}`):null;
      else state.roadDetail = parts[1] ? await api(`/v1/road-packages/${encodeURIComponent(decodeURIComponent(parts[1]))}`) : null;
      await loadRoadPackages();
    }
    render();
  } else if (state.scenes.length) await openScene(state.scenes[0].id);
  else {
    state.view = "scenes";
    render();
  }
}
window.addEventListener("hashchange", () => safely(navigate));
let resizeTimer;
window.addEventListener("resize", () => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(render, 120);
});
(async () => {
  try {
    state.health = await api("/healthz");
    await loadScenes();
    await navigate();
  } catch (e) {
    state.view = "scenes";
    render();
    notice(e.message, true);
  }
})();
configureMap(api,()=>state.token);
setInterval(() => {
  if (!state.refreshing && (!refreshBlocked() || canRefreshProgress()))
    refresh({ background: true }).catch((e) => notice(e.message, true));
}, 2500);
