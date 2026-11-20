import { list, num, esc, badge } from './schedule.js';
import { humanBytes, taskActive } from './tasks.js';

// Only these same-origin resources can receive the in-memory service token.
export function managedResource(url) {
  const actual = url.startsWith('pmtiles://') ? url.slice(10) : url;
  const parsed = new URL(actual, location.href);
  return parsed.origin === location.origin && /^\/v1\/(basemaps|basemap-assets)\//.test(parsed.pathname);
}
export function mapRequest(url, token, origins) {
  const actual = url.startsWith('pmtiles://') ? url.slice(10) : url;
  const parsed = new URL(actual, location.href);
  if (parsed.origin !== location.origin && !list(origins).includes(parsed.origin)) throw Error('地图服务域未配置');
  return { url, credentials:'omit', ...(managedResource(url) && token ? {headers:{Authorization:`Bearer ${token}`}} : {}) };
}
let api, getToken = () => '', protocol, changed;
const contexts = new Map(), styles = new Map(), sources = new Map();
export function configureBasemaps(client, token, pmtilesProtocol, onChange) {
  api = client; getToken = token; protocol = pmtilesProtocol; changed = onChange;
}
export function displayContext(view, ctx) {
  if (view.coverage) return null;
  if (ctx.tab==='data' && ctx.dirty) return null;
  if (view.op) return {operationId:view.op.id || ctx.id, version:view.op.revision || '0'};
  if (view.job?.id) return {jobId:view.job.id};
  const scene = ctx.scene?.scenario;
  return scene?.id ? {scenarioId:scene.id,version:scene.version} : null;
}
function registerArchive(url) {
  if (!protocol || sources.has(url)) return;
  const source = new globalThis.pmtiles.FetchSource(url, undefined, 'omit');
  const read = source.getBytes.bind(source);
  source.getBytes = (...args) => {
    // FetchSource bypasses MapLibre's transformRequest for internal Range reads.
    source.setHeaders(new Headers(managedResource(url) && getToken() ? {Authorization:`Bearer ${getToken()}`} : {}));
    return read(...args);
  };
  protocol.add(new globalThis.pmtiles.PMTiles(source));
  sources.set(url, source);
}
async function localStyle(url) {
  if (styles.has(url)) return styles.get(url);
  const style = await api(url);
  if (style.glyphs) style.glyphs = new URL(style.glyphs, location.href).href.replace(/%7B/g,'{').replace(/%7D/g,'}');
  if (style.sprite) style.sprite = new URL(style.sprite, location.href).href;
  for (const source of Object.values(style.sources)) {
    if (source.url?.startsWith('pmtiles://')) {
      const archive = new URL(source.url.slice(10), location.href).href;
      if (!managedResource(archive)) throw Error('非受控离线底图路径');
      registerArchive(archive);
      source.url = `pmtiles://${archive}`;
    }
  }
  styles.set(url, style);
  return style;
}
function notify() { changed?.(); }
async function refresh(entry) {
  if (entry.loading) return;
  entry.loading = true;
  try {
    const query = new URLSearchParams(entry.context);
    let selection = await api(`/v1/basemaps/selection?${query}`);
    if (selection.package && !selection.selected) {
      try {
        selection = await api('/v1/basemaps/selection', {context:selection.context,packageId:selection.package.id,expectedPackageId:selection.boundPackageId || ''});
      } catch (e) {
        if (!/changed|conflict/i.test(e.message)) throw e;
        selection = await api(`/v1/basemaps/selection?${query}`);
      }
    }
    entry.selection = selection;
    entry.preparing = selection.taskId && selection.taskId!==selection.package?.taskId
      ? taskActive(await api(`/v1/tasks/${encodeURIComponent(selection.taskId)}`)) : selection.status==='preparing';
    if (selection.package) {
      entry.light = await localStyle(selection.styleUrl);
      entry.standard = await localStyle(selection.standardStyleUrl);
      entry.error = '';
    } else {
      entry.light=null;entry.standard=null;
      if (selection.status === 'missing' && selection.automatic && !entry.attempted) {
        entry.attempted = true;
        const task = await api('/v1/basemap-packages/prepare', {context:selection.context,automatic:true,nativeMaxZoom:14});
        selection.taskId = task.id;
        selection.status = 'preparing';
      }
    }
  } catch (e) { entry.error = e.message; }
  finally {
    entry.checked = true; entry.loading = false; notify();
    if (entry.selection?.status === 'preparing' || entry.preparing) entry.timer = setTimeout(() => refresh(entry), 2000);
  }
}
export function invalidateBasemaps() {
  for (const entry of contexts.values()) {
    clearTimeout(entry.timer);
    if (entry.context) { entry.checked = false; refresh(entry); }
  }
}
export function resolveBasemap(view, ctx) {
  if (view.basemapPackage) {
    const p = view.basemapPackage, key = `package:${p.id}`;
    if (!contexts.has(key)) {
      const e = {selection:{package:p,status:'ready'}};
      contexts.set(key,e);
      Promise.all([localStyle(`/v1/basemaps/${p.id}/style-light.json`), localStyle(`/v1/basemaps/${p.id}/style-standard.json`)])
        .then(([light,standard]) => {e.light=light;e.standard=standard;})
        .catch(err => {e.error=err.message;}).finally(() => {e.checked=true;notify();});
    }
    return contexts.get(key);
  }
  const context = displayContext(view,ctx);
  if (!context) return null;
  const key = JSON.stringify(context);
  if (!contexts.has(key)) {
    const entry = {context}; contexts.set(key,entry); refresh(entry);
  }
  return contexts.get(key);
}
export function basemapStatus(entry, frame, outside) {
  let box = frame.querySelector('.offline-basemap-status');
  if (!entry) {box?.remove();return;}
  if (!box) {box=document.createElement('div');box.className='offline-basemap-status';box.setAttribute('role','status');frame.append(box);}
  const s=entry.selection, ready=!!entry.light;
  const message = outside ? '当前范围未覆盖 · 可显式准备此范围' : ready ? `离线底图可用 · 原生 ${s.package.nativeMinZoom || 0}–${s.package.nativeMaxZoom} 级` : !entry.checked ? '正在检查离线底图…' : s?.status==='preparing' ? '正在准备离线底图 · 在线底图过渡' : entry.error ? `离线底图不可用：${entry.error}` : s?.status==='failed' ? '离线底图准备失败 · 可在任务中心重试' : '当前场景未准备离线底图';
  box.replaceChildren(document.createTextNode(message));
  if (s?.taskId && !ready) {
    const task=document.createElement('button');task.dataset.action='open-generic-task';task.dataset.task=s.taskId;task.textContent='查看任务';box.append(task);
  }
  if (entry.context) {
    const button=document.createElement('button');button.textContent=outside ? '准备当前范围' : ready && Number(s.package.nativeMaxZoom)<15 ? '准备详细底图' : ready ? '底图包' : '准备底图';
    if(ready && Number(s.package.nativeMaxZoom)>=15 && !outside) {button.dataset.action='basemap-detail';button.dataset.package=s.package.id;}
    else button.onclick=() => window.dispatchEvent(new CustomEvent('prepare-basemap',{detail:{context:s?.context || entry.context,selection:s,outside}}));
    box.append(button);
  }
}
export const boundsText = bounds => list(bounds).map(b => `${num(b.west).toFixed(3)}, ${num(b.south).toFixed(3)} — ${num(b.east).toFixed(3)}, ${num(b.north).toFixed(3)}`).join('；');
export function packageCoverage(p) {
  return {type:'MultiPolygon',coordinates:list(p.bounds).map(b => [[[num(b.west),num(b.south)],[num(b.east),num(b.south)],[num(b.east),num(b.north)],[num(b.west),num(b.north)],[num(b.west),num(b.south)]]])};
}
export function renderPackageTabs(state) {
  return `<nav class="package-tabs" aria-label="地图包类型"><button data-action="package-tab" data-package-tab="roads" aria-selected="${state.packageTab!=='basemaps'}">路网包</button><button data-action="package-tab" data-package-tab="basemaps" aria-selected="${state.packageTab==='basemaps'}">底图包</button></nav>`;
}
export function renderBasemapPackages(state) {
  const packages=list(state.basemapPackages),p=state.basemapDetail;
  return `<div class="page-heading"><div><div class="eyebrow">离线显示资源</div><h1>底图包管理</h1><p>底图包用于地图显示；同一份矢量数据共用清淡和标准两套样式。</p></div><div class="actions"><button data-action="basemap-prepare" class="primary">按场景准备</button><button data-action="basemap-import">导入 PMTiles</button><button data-action="refresh">刷新</button></div></div><section class="panel"><div class="data-toolbar"><span>${packages.length} 个包 · ${humanBytes(state.basemapUsed)} / ${humanBytes(state.basemapLimit)}</span><span class="hint">自动准备原生 0–14 级；15 级需手动准备</span></div><div class="table-scroll"><table class="global-table"><thead><tr><th>名称／来源日期</th><th>覆盖范围</th><th>原生级别</th><th>大小</th><th>状态／引用</th><th>操作</th></tr></thead><tbody>${packages.map(p=>`<tr><td><strong>${esc(p.name || p.id)}</strong><small>${esc(p.source?.published || p.source?.key || '本地导入')}</small></td><td>${esc(boundsText(p.bounds))}</td><td>${num(p.nativeMinZoom)}–${num(p.nativeMaxZoom)}</td><td>${humanBytes(p.sizeBytes)}</td><td>${badge(p.enabled?'已启用':'已停用',p.enabled?'green':'orange')}<small>${num(p.referenceCount)} 个显示引用</small></td><td><div class="actions"><button data-action="basemap-detail" data-package="${esc(p.id)}">详情／预览</button><button data-action="basemap-enable" data-package="${esc(p.id)}">${p.enabled?'停用':'启用'}</button></div></td></tr>`).join('') || '<tr><td colspan="6">暂无离线底图包。打开已保存场景的地图时自动准备，也可以导入已有 PMTiles。</td></tr>'}</tbody></table></div></section>${p?`<section class="panel basemap-package-detail"><div class="panel-head"><div><h2>${esc(p.name)}</h2><p>${esc(p.attribution)}</p></div><button data-action="basemap-close">收起详情</button></div><div class="panel-pad"><dl class="road-metadata"><dt>覆盖</dt><dd>${esc(boundsText(p.bounds))}</dd><dt>格式／原生级别</dt><dd>${esc(p.format)} · ${num(p.nativeMinZoom)}–${num(p.nativeMaxZoom)}（地图可继续放大）</dd><dt>完整性</dt><dd>归档结构与 SHA-256 校验通过</dd><dt>资源版本</dt><dd>${esc(p.assetsVersion)}</dd><dt>来源快照</dt><dd>${esc(p.source?.key || '本地导入')} · ${esc(p.source?.version || '')} · ${esc(p.source?.etag || '')}</dd><dt>内容哈希</dt><dd class="hash-value">${esc(p.sha256)}</dd><dt>引用</dt><dd>${list(p.references).map(esc).join('<br>') || '暂无引用'}</dd></dl><div class="actions"><button data-action="basemap-prepare" data-level="15">准备详细底图</button><button data-action="basemap-update" data-package="${esc(p.id)}">更新相同范围</button>${p.taskId?`<button data-action="open-generic-task" data-task="${esc(p.taskId)}">关联任务</button>`:''}<button data-action="basemap-delete" data-package="${esc(p.id)}" class="danger-button" ${num(p.referenceCount)?'disabled title="仍被场景或历史视图引用"':''}>清理未引用包</button></div><div id="road-coverage-map" class="map-frame"><div class="map-host" aria-label="底图包覆盖预览"></div><div class="map-fallback"></div><div class="map-state" role="status">准备地图…</div><button class="map-fit quiet">查看覆盖范围</button></div></div></section>`:''}`;
}
