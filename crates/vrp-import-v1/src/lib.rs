//! 把 vrp-0（约束集 v1）的 JSON 转成 v2 的 `SolveRequest`。
//!
//! 只用于基准测试和历史回放，不是兼容模式。映射规则见 docs/constraints-v2.md 第 4 节。

pub mod scene;
pub mod time;

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde_json::{Map, Value};
use vrp_core::constraint::ConstraintId as C;
use vrp_model::weight_slot_mut;
use vrp_proto::engine::v1 as pb;

use time::{SECONDS_PER_DAY, format_date, parse_date, parse_datetime, parse_iso_duration};

const UNKNOWN: u32 = u32::MAX - 1;
/// 没有时间窗的工单使用的截止时刻（约 100 年）。
const FAR_FUTURE: i64 = 100 * 365 * SECONDS_PER_DAY;
/// v1 的浮点重量、体积放大成整数的倍数；对应 `penalty_unit`，罚分仍按 v1 单位计。
const QUANTITY_SCALE: f64 = 1000.0;
/// v1 旧版整数资质等级映射成的资质键。
pub const LEGACY_LEVEL_KEY: &str = "__level";
/// v1 `time_restricted_traffic_charge` 的默认值。
const DEFAULT_RESTRICTED_CHARGE: i64 = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportError(pub String);

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ImportError {}

#[derive(Debug)]
pub struct Imported {
    pub request: pb::SolveRequest,
    pub warnings: Vec<String>,
    /// v1 文件里记录的分数，只有求解结果文件才有。
    pub recorded_score: Option<String>,
    tickets: HashMap<String, u32>,
    agents: HashMap<String, u32>,
    virtual_agents: HashSet<String>,
    /// POI ID → 地点下标。
    locations: HashMap<String, u32>,
}

impl Imported {
    pub fn ticket_index(&self, id: &str) -> Option<u32> {
        self.tickets.get(id).copied()
    }

    pub fn agent_index(&self, id: &str) -> Option<u32> {
        self.agents.get(id).copied()
    }
}

/// 找到 v1 的 plan：顶层、`plan` 或 `scenario.plan`。
pub fn find_plan(v: &Value) -> Option<&Map<String, Value>> {
    let obj = v.as_object()?;
    if obj.contains_key("tickets") && obj.contains_key("agents") {
        return Some(obj);
    }
    ["plan", "scenario"]
        .into_iter()
        .find_map(|key| obj.get(key).and_then(find_plan))
}

fn id_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Object(o) => o.get("id").and_then(id_of),
        _ => None,
    }
}

fn str_of<'a>(o: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    o.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn f64_of(o: &Map<String, Value>, key: &str) -> Option<f64> {
    o.get(key).and_then(Value::as_f64).filter(|v| v.is_finite())
}

fn array_of<'a>(o: &'a Map<String, Value>, key: &str) -> &'a [Value] {
    o.get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn strings_of(o: &Map<String, Value>, key: &str) -> Vec<String> {
    array_of(o, key).iter().filter_map(id_of).collect()
}

fn quantity(x: Option<f64>) -> i64 {
    (x.unwrap_or(0.0).max(0.0) * QUANTITY_SCALE).round() as i64
}

/// v1 的 LOC 对象：`lat` 字段存的是经度、`lon` 字段存的是纬度。
fn swapped_loc(poi: &Map<String, Value>, key: &str) -> Option<(f64, f64)> {
    let loc = poi.get(key)?.as_object()?;
    let (lng, lat) = (f64_of(loc, "lat")?, f64_of(loc, "lon")?);
    if lng == 0.0 && lat == 0.0 {
        return None;
    }
    // 个别数据已经是正确顺序：纬度超出范围时说明需要换回来。
    Some(if lat.abs() > 90.0 && lng.abs() <= 90.0 {
        (lat, lng)
    } else {
        (lng, lat)
    })
}

/// POI 坐标（经度, 纬度）：优先入口坐标，其次 "经度,纬度" 文本，最后 loc。
fn poi_coords(poi: &Map<String, Value>) -> Option<(f64, f64)> {
    swapped_loc(poi, "entr_loc")
        .or_else(|| {
            let (lng, lat) = str_of(poi, "location")?.split_once(',')?;
            Some((lng.trim().parse().ok()?, lat.trim().parse().ok()?))
        })
        .or_else(|| swapped_loc(poi, "loc"))
}

/// "1hard/0medium/5soft" → Weight。负权重按 0 处理并告警。
fn parse_weight(name: &str, text: &str, warnings: &mut Vec<String>) -> Option<pb::Weight> {
    let mut w = pb::Weight::default();
    for part in text.split('/') {
        let part = part.trim();
        let (number, slot) = if let Some(n) = part.strip_suffix("hard") {
            (n, &mut w.hard)
        } else if let Some(n) = part.strip_suffix("medium") {
            (n, &mut w.medium)
        } else if let Some(n) = part.strip_suffix("soft") {
            (n, &mut w.soft)
        } else {
            warnings.push(format!("constraint {name}: cannot parse weight {text:?}"));
            return None;
        };
        let Ok(value) = number.trim().parse::<i64>() else {
            warnings.push(format!("constraint {name}: cannot parse weight {text:?}"));
            return None;
        };
        if value < 0 {
            warnings.push(format!(
                "constraint {name}: negative weight {text:?} treated as 0"
            ));
        }
        *slot = value.max(0);
    }
    Some(w)
}

fn is_zero(w: &pb::Weight) -> bool {
    w.hard == 0 && w.medium == 0 && w.soft == 0
}

struct Weights {
    proto: pb::ConstraintWeights,
    /// 旧版品牌 / 产品线约束是否开着。
    brands_or_lines: bool,
}

fn map_weights(config: Option<&Map<String, Value>>, warnings: &mut Vec<String>) -> Weights {
    let mut proto = pb::ConstraintWeights::default();
    let mut legacy = Vec::new();
    let mut brands_or_lines = false;
    for (name, value) in config.into_iter().flatten() {
        if name == "name" {
            continue;
        }
        let Some(text) = value.as_str() else {
            warnings.push(format!("constraint {name}: weight is not a string"));
            continue;
        };
        let Some(weight) = parse_weight(name, text, warnings) else {
            continue;
        };
        let target = match name.as_str() {
            "agent_is_virtual" => C::TicketUnassigned,
            "ticket_arrival_time_match_expected" => {
                legacy.push((C::TicketStartServiceTimeMatchExpected, name, weight));
                continue;
            }
            "agent_qualification_level_match_ticket" => {
                legacy.push((C::AgentQualification, name, weight));
                continue;
            }
            "agent_brands_contain_ticket_brand" | "agent_product_lines_match_ticket" => {
                brands_or_lines |= !is_zero(&weight);
                continue;
            }
            other => match C::from_name(other) {
                Some(c) => c,
                None => {
                    warnings.push(format!("unknown constraint {other} ignored"));
                    continue;
                }
            },
        };
        *weight_slot_mut(&mut proto, target) = Some(weight);
    }
    for (target, name, weight) in legacy {
        let slot = weight_slot_mut(&mut proto, target);
        match slot {
            None => *slot = Some(weight),
            Some(current) if *current != weight && !is_zero(&weight) => warnings.push(format!(
                "legacy constraint {name} ignored because {} is also configured",
                target.name()
            )),
            Some(_) => {}
        }
    }
    Weights {
        proto,
        brands_or_lines,
    }
}

fn recorded_score(root: &Value, plan: &Map<String, Value>) -> Option<String> {
    let candidates = [root.get("score"), plan.get("score")];
    candidates.into_iter().flatten().find_map(|s| match s {
        Value::String(text) => Some(text.clone()),
        Value::Object(o) => {
            let level = |k: &str| o.get(k).and_then(Value::as_i64);
            Some(format!(
                "{}hard/{}medium/{}soft",
                level("hardScore")?,
                level("mediumScore")?,
                level("softScore")?
            ))
        }
        _ => None,
    })
}

/// 地点表：按首次引用的顺序编号。
struct Locations<'a> {
    pois: HashMap<String, &'a Map<String, Value>>,
    index: HashMap<String, u32>,
    list: Vec<pb::Location>,
}

impl<'a> Locations<'a> {
    fn new(plan: &'a Map<String, Value>) -> Self {
        let pois = array_of(plan, "pois")
            .iter()
            .filter_map(|p| Some((id_of(p)?, p.as_object()?)))
            .collect();
        Locations {
            pois,
            index: HashMap::new(),
            list: Vec::new(),
        }
    }

    /// `value` 是 POI 的 ID 或内联 POI 对象。
    fn resolve(
        &mut self,
        value: Option<&'a Value>,
        owner: &str,
        warnings: &mut Vec<String>,
    ) -> Option<u32> {
        let value = value?;
        let id = id_of(value)?;
        if let Some(&i) = self.index.get(&id) {
            return Some(i);
        }
        let poi = value
            .as_object()
            .filter(|o| o.len() > 1)
            .or_else(|| self.pois.get(&id).copied());
        let coords = poi.and_then(poi_coords);
        if coords.is_none() {
            warnings.push(format!("{owner}: POI {id} has no usable coordinates"));
        }
        let (lng, lat) = coords.unwrap_or((0.0, 0.0));
        let i = self.list.len() as u32;
        self.index.insert(id.clone(), i);
        self.list.push(pb::Location { id, lng, lat });
        Some(i)
    }
}

/// 不带后缀的工程师 / 车辆标识：去掉 v1 AgentEachDay ID 末尾的 "-yyMMdd"。
fn resource_id(id: &str, date_days: Option<i64>) -> String {
    if let Some(days) = date_days {
        let date = format_date(days);
        let suffix = format!("-{}{}{}", &date[2..4], &date[5..7], &date[8..10]);
        if let Some(stripped) = id.strip_suffix(&suffix) {
            return stripped.to_string();
        }
    }
    id.to_string()
}

/// v1 `Ticket.estimateDuration`：有货物明细时按件数，否则按重量。
fn estimate_duration(t: &Map<String, Value>, kind: pb::TicketType) -> i64 {
    let per_item = if kind == pb::TicketType::Service {
        10
    } else {
        5
    };
    let items = array_of(t, "items");
    let minutes = if !items.is_empty() {
        items
            .iter()
            .filter_map(|i| i.get("value").and_then(Value::as_i64))
            .map(|v| v * per_item)
            .sum()
    } else {
        let factor = if kind == pb::TicketType::Service {
            2.0
        } else {
            1.0
        };
        (f64_of(t, "weight").unwrap_or(0.0).ceil() * 5.0 * factor).round() as i64 + 5
    };
    minutes * 60
}

fn ticket_type(text: Option<&str>) -> Option<pb::TicketType> {
    match text? {
        "Delv" => Some(pb::TicketType::Delivery),
        "Delv_BH" => Some(pb::TicketType::Pickup),
        "Inst" => Some(pb::TicketType::Service),
        _ => None,
    }
}

fn is_virtual(a: &Map<String, Value>) -> bool {
    a.get("virtual").and_then(Value::as_bool).unwrap_or(false)
}

/// 读取 v1 文件里的指派：优先 agents[].tickets 的顺序，其次工单上的 agent 字段（按到达时间排序）。
fn read_assignment(
    plan: &Map<String, Value>,
    tickets: &HashMap<String, u32>,
    agents: &HashMap<String, u32>,
    virtual_agents: &HashSet<String>,
    warnings: &mut Vec<String>,
) -> Vec<Vec<u32>> {
    let mut routes = vec![Vec::new(); agents.len()];
    let mut placed = vec![false; tickets.len()];
    for a in array_of(plan, "agents").iter().filter_map(Value::as_object) {
        let Some(&ai) = id_of(a.get("id").unwrap_or(&Value::Null)).and_then(|id| agents.get(&id))
        else {
            continue;
        };
        for t in array_of(a, "tickets") {
            match id_of(t).and_then(|id| tickets.get(&id).copied()) {
                Some(ti) if !placed[ti as usize] => {
                    placed[ti as usize] = true;
                    routes[ai as usize].push(ti);
                }
                Some(_) => {}
                None => warnings.push(format!("route of agent {ai} refers to unknown ticket {t}")),
            }
        }
    }
    let mut extra = Vec::new();
    for (order, t) in array_of(plan, "tickets")
        .iter()
        .filter_map(Value::as_object)
        .enumerate()
    {
        let Some(&ti) = id_of(t.get("id").unwrap_or(&Value::Null)).and_then(|id| tickets.get(&id))
        else {
            continue;
        };
        if placed[ti as usize] {
            continue;
        }
        let Some(agent_id) = t.get("agent").and_then(id_of) else {
            continue;
        };
        match agents.get(&agent_id) {
            Some(&ai) => {
                let arrival = str_of(t, "arrival_time")
                    .and_then(parse_datetime)
                    .unwrap_or(i64::MAX);
                extra.push((ai, arrival, order, ti));
            }
            None if virtual_agents.contains(&agent_id) => {}
            None => warnings.push(format!("ticket {ti} refers to unknown agent {agent_id}")),
        }
    }
    extra.sort_unstable();
    for (ai, _, _, ti) in extra {
        placed[ti as usize] = true;
        routes[ai as usize].push(ti);
    }
    routes
}

/// 从另一份 v1 文件读取指派，按 ID 对应到已导入的问题上（例如用 `_matrix.json` 做问题、用 `_routes.json` 做方案）。
pub fn assignment_from(
    v: &Value,
    imported: &Imported,
) -> Result<(pb::Solution, Vec<String>), ImportError> {
    let plan =
        find_plan(v).ok_or_else(|| ImportError("no plan with agents and tickets found".into()))?;
    let mut warnings = Vec::new();
    let routes = read_assignment(
        plan,
        &imported.tickets,
        &imported.agents,
        &imported.virtual_agents,
        &mut warnings,
    );
    Ok((to_solution(routes), warnings))
}

fn to_solution(routes: Vec<Vec<u32>>) -> pb::Solution {
    pb::Solution {
        routes: routes
            .into_iter()
            .enumerate()
            .filter(|(_, r)| !r.is_empty())
            .map(|(a, tickets)| pb::Route {
                agent: a as u32,
                tickets,
            })
            .collect(),
    }
}

struct RawAgent<'a> {
    json: &'a Map<String, Value>,
    id: String,
    date: Option<i64>,
    shift_start: Option<i64>,
    shift_off: Option<i64>,
}

pub fn import(v: &Value) -> Result<Imported, ImportError> {
    let plan =
        find_plan(v).ok_or_else(|| ImportError("no plan with agents and tickets found".into()))?;
    let mut warnings = Vec::new();

    let config = v
        .get("constraint_configuration")
        .or_else(|| {
            v.get("scenario")
                .and_then(|s| s.get("constraint_configuration"))
        })
        .or_else(|| plan.get("constraint_configuration"))
        .and_then(Value::as_object);
    let mut weights = map_weights(config, &mut warnings);
    let costs = v
        .get("cost_parameter")
        .or_else(|| plan.get("cost_parameter"))
        .and_then(Value::as_object);
    let overload_ratio = costs
        .and_then(|c| f64_of(c, "max_overload_ratio"))
        .unwrap_or(1.2);
    let weight_tolerance = if overload_ratio > 1.0 {
        ((overload_ratio - 1.0) * 1000.0).round() as u32
    } else {
        0
    };
    let restricted_charge = costs
        .and_then(|c| f64_of(c, "time_restricted_traffic_charge"))
        .map_or(DEFAULT_RESTRICTED_CHARGE, |x| x.max(0.0) as i64);

    // agents：先读原始时间，算出计划起点后再换算成相对秒数。
    let mut raw_agents = Vec::new();
    let mut virtual_agents = HashSet::new();
    let mut agent_index = HashMap::new();
    for a in array_of(plan, "agents").iter().filter_map(Value::as_object) {
        let Some(id) = a.get("id").and_then(id_of) else {
            warnings.push("an agent without id was skipped".into());
            continue;
        };
        if is_virtual(a) {
            virtual_agents.insert(id);
            continue;
        }
        if agent_index.contains_key(&id) {
            warnings.push(format!("duplicate agent {id} skipped"));
            continue;
        }
        let shift_start = str_of(a, "shift_start_time").and_then(parse_datetime);
        let date = str_of(a, "date")
            .and_then(parse_date)
            .or(shift_start.map(|s| s.div_euclid(SECONDS_PER_DAY)));
        agent_index.insert(id.clone(), raw_agents.len() as u32);
        raw_agents.push(RawAgent {
            json: a,
            id,
            date,
            shift_start,
            shift_off: str_of(a, "shift_off_time").and_then(parse_datetime),
        });
    }

    let ticket_json: Vec<&Map<String, Value>> = array_of(plan, "tickets")
        .iter()
        .filter_map(Value::as_object)
        .collect();
    let mut ticket_index = HashMap::new();
    let mut kept = Vec::new();
    for t in &ticket_json {
        match t.get("id").and_then(id_of) {
            Some(id) if !ticket_index.contains_key(&id) => {
                ticket_index.insert(id, kept.len() as u32);
                kept.push(*t);
            }
            Some(id) => warnings.push(format!("duplicate ticket {id} skipped")),
            None => warnings.push("a ticket without id was skipped".into()),
        }
    }

    let base_day = raw_agents
        .iter()
        .filter_map(|a| a.date)
        .chain(
            kept.iter()
                .filter_map(|t| str_of(t, "min_start_time").and_then(parse_datetime))
                .map(|s| s.div_euclid(SECONDS_PER_DAY)),
        )
        .min()
        .unwrap_or(0);
    let base = base_day * SECONDS_PER_DAY;

    let mut locations = Locations::new(plan);
    let mut agents = Vec::with_capacity(raw_agents.len());
    let mut inverted_shifts = 0;
    for raw in &raw_agents {
        let a = raw.json;
        let owner = format!("agent {}", raw.id);
        let start_location = locations.resolve(a.get("start_loc"), &owner, &mut warnings);
        let Some(start_location) = start_location else {
            return Err(ImportError(format!("{owner} has no start_loc")));
        };
        let day_start = raw.date.map_or(base, |d| d * SECONDS_PER_DAY);
        let start_time = raw.shift_start.unwrap_or(day_start) - base;
        let end_time = match raw.shift_off.map(|s| s - base) {
            Some(end) if end < start_time => {
                inverted_shifts += 1;
                None
            }
            end => end,
        };
        let mut qualification_levels: std::collections::BTreeMap<String, f64> = a
            .get("qualification_levels")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_f64()?)))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(level) = f64_of(a, "qualification_level") {
            qualification_levels.insert(LEGACY_LEVEL_KEY.into(), level);
        }
        agents.push(pb::Agent {
            id: raw.id.clone(),
            resource_id: resource_id(&raw.id, raw.date),
            day: (raw.date.unwrap_or(base_day) - base_day) as i32,
            start_location,
            end_location: start_location,
            start_time,
            end_time,
            capacity: vec![quantity(f64_of(a, "weight")), quantity(f64_of(a, "vol"))],
            capacity_tolerance_permille: vec![weight_tolerance, 0],
            max_tickets: a.get("max_ticket_num").and_then(Value::as_u64).unwrap_or(0) as u32,
            fixed_cost: f64_of(a, "fix_cost_daily").map_or(0, |x| x.max(0.0) as i64),
            restricted: a.get("restrict").and_then(Value::as_bool).unwrap_or(false),
            skills: strings_of(a, "skills"),
            qualification_levels,
            depot_id: str_of(a, "depo_id").unwrap_or_default().to_string(),
            matrix: 0,
        });
    }
    if inverted_shifts > 0 {
        warnings.push(format!(
            "{inverted_shifts} agents have shift_off_time before shift_start_time (v1 result files rewrite the start); their shift end is ignored"
        ));
    }

    let mut tickets = Vec::with_capacity(kept.len());
    let (mut estimated, mut untimed, mut brand_or_line_data) = (0, 0, 0);
    for t in &kept {
        let id = id_of(t.get("id").unwrap_or(&Value::Null)).unwrap_or_default();
        let owner = format!("ticket {id}");
        let Some(location) = locations.resolve(t.get("loc"), &owner, &mut warnings) else {
            return Err(ImportError(format!("{owner} has no loc")));
        };
        let kind = ticket_type(str_of(t, "type")).unwrap_or_else(|| {
            warnings.push(format!(
                "{owner}: unknown type {:?}, treated as service",
                t.get("type")
            ));
            pb::TicketType::Service
        });
        let duration = match str_of(t, "duration").and_then(parse_iso_duration) {
            Some(d) => d.max(0),
            None => {
                estimated += 1;
                estimate_duration(t, kind)
            }
        };
        let min_start = str_of(t, "min_start_time").and_then(parse_datetime);
        let max_end = str_of(t, "max_end_time").and_then(parse_datetime);
        if min_start.is_none() || max_end.is_none() {
            untimed += 1;
        }
        let earliest = min_start.map_or(0, |s| s - base);
        let latest = max_end.map_or(FAR_FUTURE, |s| s - base);
        let mut qualification_required: std::collections::BTreeMap<String, f64> = t
            .get("qualification_levels_required")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_f64()?)))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(level) = f64_of(t, "qualification_level").filter(|l| *l > 0.0) {
            qualification_required.insert(LEGACY_LEVEL_KEY.into(), level);
        }
        if str_of(t, "brand").is_some() || !array_of(t, "product_lines").is_empty() {
            brand_or_line_data += 1;
        }
        tickets.push(pb::Ticket {
            id,
            location,
            r#type: kind as i32,
            duration,
            earliest_start: earliest,
            latest_start: latest,
            latest_end: latest,
            day: min_start.map_or(0, |s| (s.div_euclid(SECONDS_PER_DAY) - base_day) as i32),
            demand: vec![quantity(f64_of(t, "weight")), quantity(f64_of(t, "vol"))],
            skills_required: strings_of(t, "skills_required"),
            qualification_required,
            depot_id: str_of(t, "depo_id").unwrap_or_default().to_string(),
            original_agent: None,
            pinned: t.get("pinned").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    if estimated > 0 {
        warnings.push(format!(
            "{estimated} tickets had no duration; estimated with the v1 rule"
        ));
    }
    if untimed > 0 {
        warnings.push(format!(
            "{untimed} tickets lack min_start_time or max_end_time; treated as unbounded"
        ));
    }
    if weights.brands_or_lines && brand_or_line_data > 0 {
        warnings.push(format!(
            "{brand_or_line_data} tickets carry brand/product line requirements, which v2 does not support"
        ));
    }

    // 指派与原指派。
    let routes = read_assignment(
        plan,
        &ticket_index,
        &agent_index,
        &virtual_agents,
        &mut warnings,
    );
    let recorded = kept
        .iter()
        .any(|t| t.get("original_agent").is_some_and(|v| !v.is_null()));
    if recorded {
        for (ticket, t) in tickets.iter_mut().zip(&kept) {
            ticket.original_agent = t
                .get("original_agent")
                .and_then(id_of)
                .and_then(|id| agent_index.get(&id).copied());
        }
    } else {
        for (a, route) in routes.iter().enumerate() {
            for &t in route {
                tickets[t as usize].original_agent = Some(a as u32);
            }
        }
    }
    let mut unpinned = 0;
    for ticket in &mut tickets {
        if ticket.pinned && ticket.original_agent.is_none() {
            ticket.pinned = false;
            unpinned += 1;
        }
    }
    if unpinned > 0 {
        warnings.push(format!(
            "{unpinned} pinned tickets have no original agent; unpinned"
        ));
    }

    // dep_tickets lists parents; ref_tickets lists children. Mirror records
    // describe one directed dependency, not an implicit same-agent constraint.
    let mut related_pairs = Vec::new();
    let mut precedences = Vec::new();
    let explicit_related = weights
        .proto
        .relation_tickets_same_agent
        .as_ref()
        .is_some_and(|w| !is_zero(w));
    let mut unknown_refs = 0;
    for (i, t) in kept.iter().enumerate() {
        let i = i as u32;
        for key in ["ref_tickets", "dep_tickets"] {
            for r in array_of(t, key) {
                match id_of(r).and_then(|id| ticket_index.get(&id).copied()) {
                    Some(j) if j != i => {
                        if key == "dep_tickets" {
                            precedences.push(pb::TicketPair {
                                first: j,
                                second: i,
                            });
                        } else if explicit_related {
                            related_pairs.push(pb::TicketPair {
                                first: i.min(j),
                                second: i.max(j),
                            });
                        } else {
                            precedences.push(pb::TicketPair {
                                first: i,
                                second: j,
                            });
                        }
                    }
                    Some(_) => {
                        return Err(ImportError("ticket dependency refers to itself".into()));
                    }
                    None => unknown_refs += 1,
                }
            }
        }
    }
    precedences.sort_by_key(|p| (p.first, p.second));
    precedences.dedup_by_key(|p| (p.first, p.second));
    related_pairs.sort_by_key(|p| (p.first, p.second));
    related_pairs.dedup_by_key(|p| (p.first, p.second));
    if !precedences.is_empty()
        && weights
            .proto
            .ref_ticket_after_dep_ticket
            .as_ref()
            .is_none_or(is_zero)
    {
        weights.proto.ref_ticket_after_dep_ticket = Some(pb::Weight {
            hard: 1,
            medium: 0,
            soft: 0,
        });
        warnings.push("dependency order enabled at 1 hard; mirrored ref/dep records are one finish-to-start dependency".into());
    }
    if unknown_refs > 0 {
        warnings.push(format!(
            "{unknown_refs} ref/dep ticket references point to unknown tickets"
        ));
    }

    // 矩阵：按 POI ID 取值，缺失项和 Long.MAX_VALUE 记为未知。
    let n = locations.list.len();
    let mut durations = vec![UNKNOWN; n * n];
    let mut distances = vec![UNKNOWN; n * n];
    read_matrix(plan, &locations.index, n, &mut durations, &mut distances);

    let solve_seconds = v
        .get("solve_time")
        .and_then(Value::as_f64)
        .or_else(|| {
            v.get("solve_options")
                .and_then(|o| o.get("solve_time"))
                .and_then(Value::as_str)
                .and_then(parse_iso_duration)
                .map(|s| s as f64)
        })
        .unwrap_or(30.0);

    let initial = to_solution(routes);
    let request = pb::SolveRequest {
        problem: Some(pb::Problem {
            execution: None,
            id: v.get("id").and_then(id_of).unwrap_or_default(),
            horizon: Some(pb::Horizon {
                start_unix: 0,
                timezone: String::new(),
                day_starts: Vec::new(),
                start_local: format!("{}T00:00:00", format_date(base_day)),
            }),
            locations: locations.list,
            matrices: vec![pb::TravelMatrix {
                profile: "v1".into(),
                durations: to_bytes(&durations),
                distances: to_bytes(&distances),
                sources: Vec::new(),
            }],
            capacity_dimensions: vec![
                pb::CapacityDimension {
                    name: "weight".into(),
                    unit: "v1 weight / 1000".into(),
                    penalty_unit: 1000,
                },
                pb::CapacityDimension {
                    name: "volume".into(),
                    unit: "v1 vol / 1000".into(),
                    penalty_unit: 1000,
                },
            ],
            agents,
            tickets,
            related_pairs,
            precedences,
            weights: Some(weights.proto),
            costs: Some(pb::CostParameters {
                restricted_charge_per_hour: restricted_charge,
            }),
            fallback: None,
            estimate_buffer: None,
        }),
        options: Some(pb::SolveOptions {
            time_limit_ms: (solve_seconds.max(0.0) * 1000.0) as u64,
            threads: 1,
            seed: 0,
            unimproved_stop_ms: 0,
            preset: pb::SolvePreset::Fast as i32,
            ..Default::default()
        }),
        initial_solution: Some(initial),
    };

    Ok(Imported {
        request,
        warnings,
        recorded_score: recorded_score(v, plan),
        tickets: ticket_index,
        agents: agent_index,
        virtual_agents,
        locations: locations.index,
    })
}

fn to_bytes(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// 把 v1 矩阵中能对应到地点表的条目写入 `durations` / `distances`，返回写入的条目数。
fn read_matrix(
    plan: &Map<String, Value>,
    index: &HashMap<String, u32>,
    n: usize,
    durations: &mut [u32],
    distances: &mut [u32],
) -> usize {
    let Some(data) = plan
        .get("matrix")
        .and_then(|m| m.get("data"))
        .and_then(Value::as_object)
    else {
        return 0;
    };
    let valid = |x: Option<i64>| x.filter(|x| (0..UNKNOWN as i64).contains(x));
    let mut filled = 0;
    for (from, row) in data {
        let (Some(&i), Some(row)) = (index.get(from), row.as_object()) else {
            continue;
        };
        for (to, cell) in row {
            let Some(&j) = index.get(to) else {
                continue;
            };
            let d = valid(cell.get("duration").and_then(Value::as_i64));
            let s = valid(cell.get("distance").and_then(Value::as_i64));
            if let (Some(d), Some(s)) = (d, s) {
                let k = i as usize * n + j as usize;
                durations[k] = d as u32;
                distances[k] = s as u32;
                filled += 1;
            }
        }
    }
    filled
}

/// 用另一份 v1 文件里的矩阵覆盖请求中的对应条目，返回覆盖的条目数。
///
/// v1 的求解结果文件会改写 agent 的 `shift_start_time`，不能直接当问题用；
/// 这时用请求文件做问题、用结果文件补矩阵和指派。
pub fn fill_matrix_from(
    v: &Value,
    imported: &Imported,
    request: &mut pb::SolveRequest,
) -> Result<usize, ImportError> {
    let plan =
        find_plan(v).ok_or_else(|| ImportError("no plan with agents and tickets found".into()))?;
    let problem = request
        .problem
        .as_mut()
        .ok_or_else(|| ImportError("request has no problem".into()))?;
    let n = problem.locations.len();
    let matrix = problem
        .matrices
        .first_mut()
        .ok_or_else(|| ImportError("request has no matrix".into()))?;
    let decode = |bytes: &[u8]| -> Vec<u32> {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect()
    };
    let mut durations = decode(&matrix.durations);
    let mut distances = decode(&matrix.distances);
    if durations.len() != n * n || distances.len() != n * n {
        return Err(ImportError(
            "request matrix does not match its locations".into(),
        ));
    }
    let filled = read_matrix(plan, &imported.locations, n, &mut durations, &mut distances);
    matrix.durations = to_bytes(&durations);
    matrix.distances = to_bytes(&distances);
    Ok(filled)
}
