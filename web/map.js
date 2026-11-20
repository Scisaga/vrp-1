// Persistent MapLibre controller. Only native app requests use the API client;
// Only controlled same-origin map resources receive the in-memory bearer token.
import {
  Map as LibreMap,
  Marker,
  NavigationControl,
  AttributionControl,
  addProtocol,
} from "/vendor/maplibre-gl.mjs";
import { list, esc, label } from "./schedule.js";
import { taskActive } from "./tasks.js";
import { configureBasemaps, resolveBasemap, basemapStatus, mapRequest } from './basemaps.js';

let api,
  config,
  loading,
  map,
  canvas,
  mounted,
  current,
  sceneKey,
  failed = false;
const cameras = new Map(),
  geometry = new Map(),
  snapshots = new Map(),
  selections = new Map(),
  refreshed = new Map(),
  pending = new Set(),
  invalidated = new Set(),
  discarded = new Set();
const basemapToneKey = 'vrp.map.basemapTone';
const rasterPaint = ['raster-saturation','raster-contrast','raster-brightness-min','raster-brightness-max'];
const originalRasterPaint = new Map();
let basemapTone = 'light';
try { if (localStorage.getItem(basemapToneKey) === 'standard') basemapTone = 'standard'; } catch (_) {}
let resize;
let token = () => '', offline, activeStyleKey = '', styleCamera, movingOutside = false;
let basemapFailed = false,
  styleReady = false,
  lastRouteStatus = "",
  markerSignature = "",
  markers = [];

export function configureMap(client, getToken = () => '') {
  api = client;
  token = getToken;
  const protocol = globalThis.pmtiles ? new globalThis.pmtiles.Protocol() : null;
  if (protocol) addProtocol('pmtiles', protocol.tile);
  configureBasemaps(client, getToken, protocol, () => {
    if (mounted && current) mountMap(current.view,current.ctx,current.readonly);
  });
}
export function detachMap() {
  if (map && sceneKey && styleReady)
    cameras.set(sceneKey, {
      center: map.getCenter().toArray(),
      zoom: map.getZoom(),
      bearing: map.getBearing(),
      pitch: map.getPitch(),
    });
  if (canvas?.parentNode) canvas.remove();
  resize?.disconnect();
  mounted = null;
}
export function mapSettings() {
  return !config
    ? "打开场景地图后读取配置"
    : config.disabled
      ? "底图已禁用"
      : config.offlineBasemaps ? '离线优先 · 自动准备原生 0–14 级'
      : config.styleUrl
        ? "自有样式 / PMTiles"
        : config.rasterUrl?.includes("tile.openstreetmap.org")
          ? "OpenStreetMap 在线演示"
          : "自有栅格底图";
}
function requestGeometry(jobId) {
  if (
    !jobId || discarded.has(jobId) ||
    (geometry.has(jobId) && Date.now() - (refreshed.get(jobId) || 0) < 15000) ||
    pending.has(jobId)
  )
    return;
  pending.add(jobId);
  Promise.all([
    api(`/v1/jobs/${encodeURIComponent(jobId)}/geometry`),
    snapshots.has(jobId)
      ? Promise.resolve(null)
      : api(`/v1/jobs/${encodeURIComponent(jobId)}/request`),
  ])
    .then(([value, request]) => {
      if (invalidated.has(jobId) || discarded.has(jobId)) return;
      geometry.set(jobId, value);
      refreshed.set(jobId, Date.now());
      if (request) {
        const p = request.problem || {};
        snapshots.set(jobId, {
          locations: p.locations,
          agents: p.agents,
          tickets: p.tickets,
        });
      }
      if (geometry.size > 12) {
        const oldest = geometry.keys().next().value;
        geometry.delete(oldest);
        snapshots.delete(oldest);
        refreshed.delete(oldest);
      }
      if (current) {
        draw();
        updateGeometryHint();
      }
    })
    .catch(() => {})
    .finally(() => {
      pending.delete(jobId);
      if (invalidated.delete(jobId)) requestGeometry(jobId);
    });
}
export function invalidateGeometry(jobId) {
  if (!jobId) return;
  geometry.delete(jobId);
  refreshed.delete(jobId);
  if (pending.has(jobId)) {
    invalidated.add(jobId);
    return;
  }
  requestGeometry(jobId);
}
export function forgetGeometry(jobId) {
  discarded.add(jobId);
  while (discarded.size > 256) discarded.delete(discarded.values().next().value);
  geometry.delete(jobId);
  snapshots.delete(jobId);
  refreshed.delete(jobId);
  invalidated.delete(jobId);
}
export function mountMap(view, ctx, readonly = false) {
  current = { view, ctx, readonly };
  mounted = view.coverage
    ? document.getElementById('road-coverage-map')
    : document.querySelector('#scene-content .map-frame');
  if (!mounted) return;
  const frame = mounted;
  frame.querySelector(".map-fit").onclick = () => fit();
  mountBasemapControl(frame);
  const key = `${ctx.id || ctx.scene?.scenario?.id || "scene"}:${readonly ? "history" : "live"}`;
  const changed = key !== sceneKey;
  if (changed) movingOutside=false;
  sceneKey = key;
  const selection = `${!!ctx.mapOverview}|${ctx.selectedTicket || ""}|${ctx.selectedAgent || ""}|${view.locationFocus ?? ""}`,
    selectionChanged = selections.get(key) !== selection;
  selections.set(key, selection);
  const id = view.job?.id || view.op?.plan?.jobId;
  requestGeometry(id);
  updateGeometryHint();
  if (ctx.compare) requestGeometry(view.baselineJobId);
  if (!config) {
    loading ||= api("/v1/map-config")
      .then((v) => {
        config = v;
      })
      .catch(() => {
        config = { disabled: true };
      });
    loading.then(() => {
      if (current && mounted === frame)
        mountMap(current.view, current.ctx, current.readonly);
    });
    return;
  }
  if (config.disabled || failed) {
    frame.classList.add("schematic");
    status(
      config.disabled
        ? "底图未启用 · 访问顺序示意"
        : "WebGL 不可用 · 访问顺序示意",
    );
    return;
  }
  offline = config.offlineBasemaps || view.basemapPackage ? resolveBasemap(view,ctx) : null;
  basemapStatus(offline,frame,movingOutside);
  // Resolve local availability before creating a map: a warm package must not
  // cause even one temporary request to the online fallback on a cold reload.
  if (offline && !offline.checked) return;
  const styleKey = offline?.light && !movingOutside ? `${offline.selection.package.id}:${basemapTone}` : 'configured';
  const initialStyle = offline?.light && !movingOutside ? offline[basemapTone] : configuredStyle();
  if (!map) {
    canvas = document.createElement("div");
    canvas.className = "map-canvas";
    frame.querySelector(".map-host").append(canvas);
    try {
      activeStyleKey = styleKey;
      map = new LibreMap({
        container: canvas,
        style: initialStyle,
        center: offline?.selection?.package?.bounds?.length ? [(Number(offline.selection.package.bounds[0].west || 0)+Number(offline.selection.package.bounds[0].east || 0))/2,(Number(offline.selection.package.bounds[0].south || 0)+Number(offline.selection.package.bounds[0].north || 0))/2] : [0,0],
        zoom: offline?.light ? 10 : 1,
        attributionControl: false,
        localIdeographFontFamily: false,
        maxZoom: config.maxZoom || 19,
        transformRequest: url => mapRequest(url,token(),config.resourceOrigins),
      });
      map.addControl(
        new AttributionControl({
          compact: false,
          customAttribution: offline?.light ? '' : config.rasterUrl?.includes(
            "tile.openstreetmap.org",
          )
            ? '<a href="https://www.openstreetmap.org/copyright" target="_blank" rel="noopener">© OpenStreetMap contributors</a>'
            : esc(config.attribution || ""),
        }),
      );
      map.addControl(
        new NavigationControl({ showCompass: false }),
        "top-right",
      );
      map.on("load", () => draw());
      map.on("style.load", () => {
        styleReady = true;
        originalRasterPaint.clear();
        applyBasemapTone();
        mounted?.classList.remove("loading-map");
        map.resize();
        draw();
        if (styleCamera) {const camera=styleCamera;styleCamera=null;map.jumpTo(camera);}
        else restoreOrFit();
      });
      // A remote style must not hide the business schematic indefinitely.
      if (config.styleUrl)
        setTimeout(() => {
          if (!styleReady) {
            basemapFailed = true;
            map.setStyle({ version: 8, sources: {}, layers: [] });
          }
        }, 8000);
      map.on("error", () => {
        basemapFailed = true;
        status("底图暂不可用 · " + (lastRouteStatus || "业务图层仍可操作"));
        if (!map.getStyle()?.layers?.length)
          map.setStyle({ version: 8, sources: {}, layers: [] });
      });
      map.on('moveend', () => {
        if (!offline?.light || !mounted) return;
        const p=map.getCenter(),bounds=offline.selection.package.bounds || [];
        const lng=((p.lng+180)%360+360)%360-180;
        const outside=!bounds.some(b=>lng>=Number(b.west || 0)&&lng<=Number(b.east || 0)&&p.lat>=Number(b.south || 0)&&p.lat<=Number(b.north || 0));
        basemapStatus(offline,mounted,outside);
        if (outside!==movingOutside) {movingOutside=outside;mountMap(current.view,current.ctx,current.readonly);}
      });
      map.on("click", (e) => {
        const layers = ["vrp-points", "vrp-lines", "vrp-lines-context"].filter((id) =>
          map.getLayer(id),
        );
        const feature = map.queryRenderedFeatures(e.point, { layers })[0];
        if (feature) select(feature.properties);
      });
      map.on("mousemove", (e) => {
        if (map.getLayer("vrp-points"))
          map.getCanvas().style.cursor = map.queryRenderedFeatures(e.point, {
            layers: ["vrp-points", "vrp-lines"],
          }).length
            ? "pointer"
            : "";
      });
    } catch (_) {
      failed = true;
      canvas.remove();
      frame.classList.add("schematic");
      status("WebGL 不可用 · 访问顺序示意");
      return;
    }
  } else {
    frame.querySelector(".map-host").append(canvas);
    if (styleKey !== activeStyleKey) {
      activeStyleKey=styleKey;
      styleCamera=changed ? null : {center:map.getCenter().toArray(),zoom:map.getZoom(),bearing:map.getBearing(),pitch:map.getPitch()};
      basemapFailed=false;styleReady=false;
      map.setStyle(initialStyle,{diff:false});
    }
    map.resize();
    draw();
    if (changed) restoreOrFit();
    else if (selectionChanged) fit();
  }
  frame.classList.add("interactive");
  frame.classList.toggle("loading-map", !styleReady);
  resize?.disconnect();
  resize = new ResizeObserver(() => map?.resize());
  resize.observe(frame);
}
function configuredStyle() {
  return config.styleUrl || {version:8,sources:{basemap:{type:'raster',tiles:[config.rasterUrl],tileSize:256,maxzoom:config.maxZoom || 19}},layers:[{id:'basemap',type:'raster',source:'basemap'}]};
}
export function mapViewportBounds() {
  if (!map) return [];
  const b=map.getBounds(),south=Math.max(-85.0511287798,b.getSouth()),north=Math.min(85.0511287798,b.getNorth());
  if (b.getEast()-b.getWest()>=360) return [{west:-180,east:180,south,north}];
  const norm=x=>((x+180)%360+360)%360-180,west=norm(b.getWest()),east=norm(b.getEast());
  return west<=east ? [{west,east,south,north}] : [{west,east:180,south,north},{west:-180,east,south,north}];
}
function status(text) {
  if (mounted) mounted.querySelector(".map-state").textContent = text;
}
function mountBasemapControl(frame) {
  let toolbar = frame.querySelector('.map-toolbar');
  if (!toolbar) {
    toolbar = document.createElement('div');
    toolbar.className = 'map-toolbar';
    toolbar.append(frame.querySelector('.map-fit'));
    const tones = document.createElement('div');
    tones.className = 'map-tone-control';
    tones.setAttribute('role','group');
    tones.setAttribute('aria-label','底图样式');
    for (const [value,label] of [['light','清淡'],['standard','标准']]) {
      const button = document.createElement('button');
      button.type = 'button';
      button.dataset.basemapTone = value;
      button.textContent = label;
      button.onclick = () => {
        basemapTone = value;
        try { localStorage.setItem(basemapToneKey,value); } catch (_) {}
        if (offline?.light) mountMap(current.view,current.ctx,current.readonly);
        else applyBasemapTone();
      };
      tones.append(button);
    }
    toolbar.append(tones);
    frame.append(toolbar);
  }
  updateBasemapControl();
}
function updateBasemapControl() {
  if (!mounted) return;
  mounted.dataset.basemapTone = basemapTone;
  const vectorOnly = !offline?.light && styleReady && !map?.getStyle()?.layers?.some(layer => layer.type === 'raster');
  for (const button of mounted.querySelectorAll('[data-basemap-tone]')) {
    button.setAttribute('aria-pressed',String(button.dataset.basemapTone === basemapTone));
    button.disabled = vectorOnly;
    button.title = vectorOnly ? '自有矢量底图保留原配色' : button.dataset.basemapTone === 'light'
      ? '降低底图饱和度和对比度，突出路线' : '恢复底图原配色';
  }
}
function applyBasemapTone() {
  if (map && styleReady) {
    // Keep provider URLs and business layers intact, and restore a custom
    // raster style's original paint exactly when Standard is selected.
    for (const layer of map.getStyle()?.layers || []) {
      if (layer.type !== 'raster') continue;
      if (!originalRasterPaint.has(layer.id))
        originalRasterPaint.set(layer.id,Object.fromEntries(rasterPaint.map(name => [name,map.getPaintProperty(layer.id,name) ?? null])));
      const paint = basemapTone === 'light'
        ? {'raster-saturation':-0.7,'raster-contrast':-0.1,'raster-brightness-min':0.4,'raster-brightness-max':1}
        : originalRasterPaint.get(layer.id);
      for (const name of rasterPaint) map.setPaintProperty(layer.id,name,paint[name]);
    }
  }
  updateBasemapControl();
}
function updateGeometryHint() {
  const caption = mounted?.parentElement?.querySelector('.map-caption');
  if (!caption || !current) return;
  caption.querySelector('[data-road-plan-hint]')?.remove();
  const {view,ctx,readonly} = current;
  const id = view.job?.id;
  if (readonly || ctx.op || view.op || !id || !geometry.has(id) || list(geometry.get(id)?.legs).length) return;
  const hint = document.createElement('div');
  hint.dataset.roadPlanHint = '';
  const pending = ctx.geoTask && taskActive(ctx.geoTask);
  const preparedHere = ctx.geoTask?.resultJobId === id;
  hint.textContent = pending ? preparedHere ? '道路路径准备中，完成后自动显示。' : '新的道路规划正在生成，历史方案保持访问顺序示意。' : preparedHere
    ? '本方案暂无可用道路路径，虚线为几何估算示意。'
    : '此方案未包含道路路径，当前显示访问顺序示意。';
  if (!pending) {
    const button = document.createElement('button');
    button.className = 'quiet';
    button.dataset.action = 'geo-plan';
    button.textContent = '生成道路矩阵并重新规划';
    hint.append(document.createElement('br'),button);
  }
  caption.append(hint);
}
function select(p) {
  if (!mounted) return;
  const b = document.createElement("button");
  b.hidden = true;
  if (p.ticket) b.dataset.selectTicket = p.ticket;
  else if (p.agent) b.dataset.selectAgent = p.agent;
  else return;
  if (p.agent) b.dataset.agent = p.agent;
  mounted.append(b);
  b.click();
  b.remove();
}
function features() {
  if (!current) return [];
  const { view, ctx } = current,
    input = view.input || view.business,
    display = view.display || {};
  const job = view.job?.id || view.op?.plan?.jobId;
  const p = snapshots.get(job) || input?.problem || {};
  const positions = new Map(
    list(view.op?.vehicles).map((v) => [v.agentId, v.position]),
  );
  const locations = new Map(list(p.locations).map((l) => [l.id, l]));
  for (const l of list(view.business?.problem?.locations))
    if (!locations.has(l.id)) locations.set(l.id, l);
  const overview = !!ctx.mapOverview;
  const selected = overview ? "" : ctx.selectedAgent || list(view.routes)[0]?.agentId;
  const road = new Map(
    list(geometry.get(job)?.legs).map((l) => [
      `${l.agentId}:${l.legIndex || 0}`,
      l,
    ]),
  );
  const output = [],
    valid = (l) =>
      l &&
      Number.isFinite(+l.lng) &&
      Number.isFinite(+l.lat) &&
      Math.abs(+l.lng) <= 180 &&
      Math.abs(+l.lat) <= 90;
  const point = (l, props) => {
    if (valid(l))
      output.push({
        type: "Feature",
        geometry: { type: "Point", coordinates: [+l.lng, +l.lat] },
        properties: { overview, ...props },
      });
  };
  if (view.coverage) {
    const polygons = view.coverage.type === 'Polygon' ? [view.coverage.coordinates] : view.coverage.type === 'MultiPolygon' ? view.coverage.coordinates : [];
    for (const polygon of polygons) for (const ring of polygon) {
      const coordinates = continuousLine(ring.filter(p => valid({ lng:p[0], lat:p[1] })));
      if (coordinates.length > 1) output.push({ type:'Feature', geometry:{ type:'LineString', coordinates }, properties:{ road:true, coverage:true, color:'#006E85', opacity:0.9 } });
    }
    return output;
  }
  if (view.locationFocus !== undefined) {
    const location = list(p.locations)[view.locationFocus];
    point(location, { kind: "location", selected: true, color: "#FF992F", name: label(display, "locations", location?.id) });
    return output;
  }
  if (ctx.compare) {
    const bp = snapshots.get(view.baselineJobId) || view.business?.problem || p;
    const bRoad = new Map(
      list(geometry.get(view.baselineJobId)?.legs).map((l) => [
        `${l.agentId}:${l.legIndex || 0}`,
        l,
      ]),
    );
    for (const route of list(view.baseline)) {
      const a = list(bp.agents).find((a) => a.id === route.agentId);
      if (!a) continue;
      const locs = [
        bp.locations[a.startLocation || 0],
        ...list(route.stops).map((s) =>
          list(bp.locations).find((l) => l.id === s.locationId),
        ),
        bp.locations[a.endLocation || 0],
      ];
      for (let i = 0; i + 1 < locs.length; i++) {
        if (!valid(locs[i]) || !valid(locs[i + 1])) continue;
        const leg = bRoad.get(`${a.id}:${i}`),
          matches =
            leg &&
            leg.fromLocationId === locs[i].id &&
            leg.toLocationId === locs[i + 1].id;
        appendLeg(output, matches ? leg : null, locs[i], locs[i + 1], {
            agent: a.id,
            color: "#93A2B4",
            opacity: a.id === selected ? 0.8 : overview ? 0.55 : 0.15,
            baseline: true,
        });
      }
    }
  }
  for (const route of list(view.routes)) {
    const agent = list(p.agents).find((a) => a.id === route.agentId);
    if (!agent) continue;
    const stops = list(route.stops),
      locs = [
        p.locations[agent.startLocation || 0],
        ...stops.map(
          (s) =>
            locations.get(s.locationId) ||
            p.locations[
              list(p.tickets).find((t) => t.id === s.ticketId)?.location || 0
            ],
        ),
        p.locations[agent.endLocation || 0],
      ];
    const color = agent.id === selected ? '#006478' :
      view.job &&
      ![undefined, "JOB_KIND_PLANNING", 0].includes(view.job.kind) &&
      view.op?.plan?.jobId !== view.job.id
        ? "#FF992F"
        : overview ? "#007B94" : "#638F99";
    for (let i = 0; i + 1 < locs.length; i++) {
      if (!valid(locs[i]) || !valid(locs[i + 1])) continue;
      const leg = road.get(`${agent.id}:${i}`),
        matches =
          leg &&
          leg.fromLocationId === locs[i].id &&
          leg.toLocationId === locs[i + 1].id;
      appendLeg(output, matches ? leg : null, locs[i], locs[i + 1], {
          agent: agent.id,
          color,
          selected: agent.id === selected,
          opacity: agent.id === selected ? 1 : overview ? 0.8 : 0.2,
      });
    }
    point(locs[0], {
      agent: agent.id,
      kind: "depot",
      color: "#006E85",
      name: "起点",
    });
    point(locs.at(-1), {
      agent: agent.id,
      kind: "depot",
      color: "#006E85",
      name: "终点",
    });
    for (const [index, stop] of stops.entries()) {
      const t = list(p.tickets).find((t) => t.id === stop.ticketId);
      point(
        locations.get(stop.locationId) || (t && p.locations[t.location || 0]),
        {
          ticket: stop.ticketId,
          agent: agent.id,
          kind: "ticket",
          sequence: index + 1,
          selected: agent.id === selected,
          color: ctx.selectedTicket === stop.ticketId ? "#FF992F" : color,
          name: label(display, "tickets", stop.ticketId),
        },
      );
    }
  }
  const assigned = new Set(
    output.map((f) => f.properties.ticket).filter(Boolean),
  );
  for (const ticket of list(p.tickets))
    if (!assigned.has(ticket.id))
      point(p.locations[ticket.location || 0], {
        ticket: ticket.id,
        kind: "ticket",
        color: ticket.id === ctx.selectedTicket ? "#FF992F" : "#748296",
        name: label(display, "tickets", ticket.id),
      });
  for (const [agent, position] of positions)
    point(position, {
      agent,
      kind: "vehicle",
      color: "#FF992F",
      name: "车辆当前位置",
    });
  return output;
}
function draw() {
  // isStyleLoaded waits for raster tiles too. Business layers only require
  // the parsed style, and must work while the basemap is slow or unavailable.
  if (!map || !styleReady) return;
  const data = { type: "FeatureCollection", features: features() };
  if (!map.getSource("vrp")) {
    map.addSource("vrp", { type: "geojson", data });
    const lineDash = ["case",["get","road"],["literal",[1,0]],["literal",[2,2]]];
    map.addLayer({
      id: "vrp-lines-context",
      type: "line",
      source: "vrp",
      filter: ["all",["==", ["geometry-type"], "LineString"],["!=",["get","selected"],true]],
      layout: {'line-cap':'round','line-join':'round'},
      paint: {
        "line-color": ["get", "color"],
        "line-opacity": ["get", "opacity"],
        "line-width": 2.2,
        "line-dasharray": lineDash,
      },
    });
    map.addLayer({
      id:'vrp-lines-halo',type:'line',source:'vrp',
      filter:['all',['==',['geometry-type'],'LineString'],['==',['get','selected'],true],['==',['get','road'],true]],
      layout:{'line-cap':'round','line-join':'round'},
      paint:{'line-color':'#ffffff','line-opacity':0.96,'line-width':7.5},
    });
    map.addLayer({
      id:'vrp-lines',type:'line',source:'vrp',
      filter:['all',['==',['geometry-type'],'LineString'],['==',['get','selected'],true]],
      layout:{'line-cap':'round','line-join':'round'},
      paint:{'line-color':['get','color'],'line-opacity':1,'line-width':4.5,'line-dasharray':lineDash},
    });
    map.addLayer({
      id: "vrp-points",
      type: "circle",
      source: "vrp",
      filter: ["==", ["geometry-type"], "Point"],
      paint: {
        "circle-color": ["get", "color"],
        "circle-radius": ["case", ["==", ["get", "kind"], "vehicle"], 8, ["==", ["get", "overview"], true], 3.2, 5],
        "circle-stroke-width": ["case", ["==", ["get", "overview"], true], 1.2, 2],
        "circle-stroke-color": "#fff",
      },
    });
  } else map.getSource("vrp").setData(data);
  const roads = data.features.filter(
      (f) => f.properties.road && !f.properties.baseline,
    ).length,
    lines = data.features.filter(
      (f) => f.geometry.type === "LineString" && !f.properties.baseline,
    ).length;
  const routeStatus = current.view.coverage ? '源包声明的覆盖范围' : roads
    ? `道路路径已加载${roads < lines ? ' · 虚线为接入连线或估算示意' : ''}`
    : '访问顺序示意 · 尚无可用道路路径';
  lastRouteStatus = routeStatus;
  status((basemapFailed ? "底图暂不可用 · " : "") + routeStatus);
  const numbered = data.features.filter(
    (f) => f.properties.sequence && f.properties.selected,
  );
  const signature = JSON.stringify(numbered);
  if (signature !== markerSignature) {
    markers.forEach((marker) => marker.remove());
    markers = numbered.map((f) => {
      const button = document.createElement("button");
      button.className = "map-stop-label";
      button.textContent = f.properties.sequence;
      button.title = `${f.properties.sequence}. ${f.properties.name}`;
      button.onclick = (e) => {
        e.stopPropagation();
        select(f.properties);
      };
      return new Marker({ element: button })
        .setLngLat(f.geometry.coordinates)
        .addTo(map);
    });
    markerSignature = signature;
  }
  // Accessible selection list also supports keyboard operation without WebGL picking.
  const old = mounted?.querySelector(".map-object-list");
  old?.remove();
  if (mounted) {
    const box = document.createElement("div");
    box.className = "map-object-list";
    const unique = new Set();
    for (const f of data.features.filter((f) => f.geometry.type === "Point")) {
      const p = f.properties,
        id = p.ticket || p.agent;
      if (!id || unique.has(id)) continue;
      unique.add(id);
      const button = document.createElement("button");
      button.textContent = p.name || id;
      button.onclick = () => select(p);
      box.append(button);
    }
    mounted.append(box);
  }
}
function continuousLine(points) {
  const result = [];
  for (const point of points) {
    let longitude = +point[0];
    if (result.length) longitude += 360 * Math.round((result[result.length - 1][0] - longitude) / 360);
    result.push([longitude, +point[1]]);
  }
  return result;
}
function appendLeg(output, leg, from, to, properties) {
  const coordinates = continuousLine(leg ? list(leg.points).map(p => [+p.lng,+p.lat]) : [[+from.lng,+from.lat],[+to.lng,+to.lat]]);
  const add = (points, road, connector = false) => {
    if (points.length > 1) output.push({ type:'Feature', geometry:{type:'LineString',coordinates:points}, properties:{...properties,road,connector} });
  };
  const start = leg?.roadStartPointIndex, end = leg?.roadEndPointIndex;
  if (leg && start !== undefined && end !== undefined && Number.isInteger(+start) && Number.isInteger(+end) && +start <= +end && +end < coordinates.length) {
    add(coordinates.slice(0,+start + 1), false, true);
    add(coordinates.slice(+start,+end + 1), true);
    add(coordinates.slice(+end), false, true);
  } else add(coordinates, !!leg);
}
function fit() {
  if (!map) return;
  const all = features(),
    selected = current?.ctx.selectedTicket || current?.ctx.selectedAgent;
  let values = all.filter(
    (f) =>
      !selected ||
      f.properties.ticket === selected ||
      f.properties.agent === selected,
  );
  if (!values.length) values = all;
  const points = values.flatMap((f) =>
    f.geometry.type === "Point"
      ? [f.geometry.coordinates]
      : f.geometry.coordinates,
  );
  if (!points.length) return;
  const bounds = [
    [Infinity, Infinity],
    [-Infinity, -Infinity],
  ];
  const longitudeRange = points.reduce((r,p) => [Math.min(r[0],p[0]),Math.max(r[1],p[0])], [Infinity,-Infinity]);
  const crossesDateLine = longitudeRange[1] - longitudeRange[0] > 180;
  for (let [x, y] of points) {
    if (crossesDateLine && x < 0) x += 360;
    bounds[0][0] = Math.min(bounds[0][0], x);
    bounds[0][1] = Math.min(bounds[0][1], y);
    bounds[1][0] = Math.max(bounds[1][0], x);
    bounds[1][1] = Math.max(bounds[1][1], y);
  }
  map.fitBounds(bounds, { padding: 35, maxZoom: 15, duration: 0 });
}
function restoreOrFit() {
  const camera = cameras.get(sceneKey);
  if (camera) map.jumpTo(camera);
  else fit();
}
