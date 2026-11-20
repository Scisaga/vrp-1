# 约束集 v2

本文定义 vrp-1 的评分语义，是 `vrp-core` 评分实现和测试的唯一依据。v2 以 vrp-0（约束集 v1）为基础，修正了已知的语义问题；每条约束都注明了与 v1 的差异。

## 1. 通用规则

**分数。** `Score(hard, medium, soft)`，各级别都是罚分之和（≤ 0），按 hard、medium、soft 依次比较，越大越好。

**计分。** 每条约束先算出非负整数的"匹配量"（amount），再乘以该约束的权重 `Weight(hard, medium, soft)`，取负后计入对应级别。一条约束可以同时在多个级别计分。

**权重。** 请求里未设置的约束用下表的默认权重；显式设置为全 0 表示关闭。各级别取值范围 0 到 10⁹。

**数值范围。** 编译时使用 i128 估计全体工单在任意合法指派下的载荷、时间、匹配量和各级总罚分上界。上界超过 `i64::MAX / 4` 的请求返回输入错误，为增量加减保留余量；不会用溢出回绕或饱和计分替代真实分数。这个检查是保守的，极端输入即使个别方案可计分也可能被拒绝。

**已派与未派。** 工单要么在某个 agent 的路线上，要么未派。v1 用"虚拟 agent"承接未派工单，v2 取消这一概念。未派工单计 `ticket_unassigned`；有原指派的还计 `minimize_ticket_changing`，不参与路线或 agent 级计算。

**固定工单。** `pinned` 工单必须出现在 `original_agent` 的路线中，不能取消或改派，路线内顺序可以调整。这是结构约束，与评分权重无关。显式初始解违反此规则时拒绝输入；不提供初始解时，构造种子方案先放入所有固定工单。

**未使用的 agent。** 没有派单的 agent 不计任何 agent 级罚分（固定成本、均衡类等）。

**时间推算。**
- agent 在 `start_time` 从 `start_location` 出发；
- 到达 = 上一站离开时刻 + 这条边的排班时长（见下条）；
- 启用时序依赖时，开始服务 = max(到达, `earliest_start`, 全部前置工单结束时刻)；多个前置为 AND，不要求同一个 agent。已完成/服务中的前置使用执行投影提供的完成边界；没有前置时保持原推时规则。
- 结束服务 = 开始服务 + `duration`，也就是离开时刻；
- 最后一单结束后返回 `end_location`，回到终点的时刻就是路线结束时刻。回程计入行驶时长和距离。

**前置依赖。** `precedences` 是有向无环图；自引用、越界引用和循环依赖在编译时拒绝，重复边去重。时序约束默认 1 hard，显式全 0 仍表示关闭跨工单等待和相应评分。工作台新增依赖、历史依赖语义迁移时，若原权重为 0 则明确启用为 1 hard。前置未派而后续已派，每条边计 1 个时序违约；后续未派不计。搜索中的路线顺序可能与依赖形成环，此时使用不加跨工单等待的有限基础时序，并按无法拓扑排程的已派工单数额外计时序违约，避免等待无限增长；这种方案不能采用。正常无环方案按路线顺序和依赖边联合拓扑推时，等待会传播到后续站点和其他资源。显式历史同资源约束独立保留，不从依赖关系推导。

**估算边的排班余量。** 来源为估算（`EDGE_SOURCE_MODEL`、`EDGE_SOURCE_FALLBACK`）的边可能低估实际行驶时间，推算时刻时按放大后的时长计：
- 排班时长 = ⌈行驶时长 × (1000 + p) / 1000⌉，p 是这条边的余量千分比；
- p 取 `Problem.estimate_buffer` 中对应来源的值。不给 `estimate_buffer` 时，MODEL 默认 100（放大 10%），FALLBACK 默认 200（放大 20%）；
- 实测（EXACT）和缓存（CACHED）的边 p = 0。

排班时长只影响到达、开始、结束等时刻，以及由时刻决定的约束（迟到、计划日、依赖、班次超时）。按行驶时长计量的约束（`minimize_travel_time`、限行费、`balance_agent_working_time`）仍用原始时长。

**载荷。**
- 出发时的载荷 = 路线上所有 DELIVERY 工单的需求之和；
- 途经 DELIVERY 站点卸货（减去需求），PICKUP 站点装货（加上需求），SERVICE 站点不变；
- 每个容量维度分别求全程峰值（包括出发时刻）。

**分钟类匹配量。** 秒差向上取整为分钟，不足 1 分钟按 1 分钟计。v1 用 `Duration.toMinutes()` 截断，59 秒以内的迟到不计罚，搜索也就看不见它。

**不可达边。** 矩阵里标为不可达的边计 `unreachable_leg`，其行驶时长和距离按 0 计。

**日期。** 某时刻所在的"日"取 Horizon 的日序号，不做日历计算。

## 2. 约束列表

| # | 约束 | 默认权重 | 作用对象 | 匹配量 | 与 v1 的差异 |
|---|---|---|---|---|---|
| 1 | `agent_capacity` | 1 hard | 已使用的 agent | Σ 各维度 ⌈max(0, 峰值 − 上限) / penalty_unit⌉，上限 = ⌊容量 × (1000 + 容差千分比) / 1000⌋ | v1 只在"重量峰值那一刻"比较，并把千克和立方米相加；v2 各维度独立求峰值、分别取整；超载容差可按维度设置（v1 只作用于重量） |
| 2 | `agent_max_ticket` | 1 hard | 已使用的 agent | max(0, 单数 − `max_tickets`)；`max_tickets` = 0 不限 | 无 |
| 3 | `agent_skills_accord_with_ticket_skills` | 1 hard | 已派工单 | 工单要求的技能不全在 agent 技能中计 1 | 无 |
| 4 | `agent_qualification_levels_match_ticket` | 100 medium | 已派工单 | 任一要求的资质缺失或等级低于要求计 1 | 无 |
| 5 | `ref_ticket_after_dep_ticket` | 1 hard | 已派后续及其前置 | 两张都已派时 ⌈max(0, 前置结束 − 后续开始) / 60⌉；前置未派计 1；路线/依赖联合图不能拓扑排程时另计受阻工单数 | 等待所有前置完成；未派前置不能使已派后续绕过依赖；旧版仅比较到达时刻 |
| 6 | `ref_ticket_same_agent_with_dep_ticket` | 关闭 | 两张都已派的依赖对 | agent 不同计 1 | v1 一张已派一张未派也计 1 |
| 7 | `service_finished_after_max_end_time` | 关闭 | 已派工单 | ⌈max(0, 结束服务 − `latest_end`) / 60⌉ | v1 用"到达 + 时长"，忽略了等待；v2 用实际开始服务时刻 |
| 8 | `ticket_start_service_time_match_expected` | 100 soft | 已派工单 | ⌈max(0, 开始服务 − `latest_start`) / 60⌉ | 截断改为向上取整 |
| 9 | `ticket_arrival_time_same_date_with_plan_time` | 1 hard | 已派工单 | 开始服务所在日 ≠ 工单计划日计 1 | v1 比较 `getDayOfYear()`，跨年会误判 |
| 10 | `relation_tickets_same_agent` | 50 medium | 两张都已派的关联对 | agent 不同计 1 | v1 按有向引用计数，A、B 互相引用时同一对计 2 次；v2 按无序对计 1 次。互相引用的数据要保持原有力度，需把权重翻倍 |
| 11 | `minimize_travel_time` | 1 soft | 已使用的 agent | 行驶秒数（含回程） | 无 |
| 12 | `minimize_travel_distance` | 关闭 | 已使用的 agent | 行驶米数（含回程） | 无 |
| 13 | `minimize_agent_fixed_cost` | 20 soft | 已使用的 agent | `fixed_cost`，限行车辆另加 ⌊`restricted_charge_per_hour` × 行驶秒数 / 3600⌋ | v1 对所有 agent（包括没派单的）计固定成本，结果只是常数项；v1 的限行费写死为每 2 小时 100，v2 改读参数 |
| 14 | `same_depo` | 1 hard | 已派工单 | 工单与 agent 的网点都非空且不同计 1 | 无 |
| 15 | `balance_agent_loading` | 1 medium | 已使用的 agent | 单数² | 无 |
| 16 | `balance_agent_loading_ratio` | 关闭 | 已使用的 agent | ⌊max(10 × Σ 各维度 配送需求/容量, 1)²⌋，容量为 0 的维度跳过 | v1 只看配送（Delv）工单，这一点不变；v1 容量为 0 时除零，结果溢出；v1 对没派单的 agent 也计 1 |
| 17 | `balance_agent_working_time` | 关闭 | 已使用的 agent | ⌈(行驶秒数 + 服务秒数) / 3600⌉² | 无 |
| 18 | `minimize_ticket_changing` | 1000 soft | 有原指派的工单 | 当前 agent 与原 agent 不同（包括变成未派）计 1 | 无 |
| 19 | `ticket_unassigned` | 1000 medium | 未派工单 | 1 | v1 名为 `agent_is_virtual`（工单挂在虚拟 agent 上） |
| 20 | `unreachable_leg` | 1 hard | 已使用的 agent | 经过的不可达边数 | 新增：v1 遇到取不到的边时，行驶成本直接跳过（相当于免费），而到达时间又算不出来 |
| 21 | `agent_shift_overtime` | 100 soft | 已使用、且给出 `end_time` 的 agent | ⌈max(0, 路线结束 − `end_time`) / 60⌉ | 新增：v1 不检查班次结束，只有计划日约束能间接拦住跨过午夜的路线 |
| 22 | `execution_consistency` | 1 hard（执行上下文必须保持此权重） | 执行路线 | 各站负载荷绝对量 + 缺少同车在先装货次数 + 承诺时间范围违约分钟 | 新增物理执行边界 |
| 23 | `minimize_visit_order_changing` | 10 soft | 有顺序基准的业务单 | 在剩余业务单中的位置变化计 1，不计装卸辅助站 | 新增计划稳定性 |
| 24 | `minimize_service_time_changing` | 1 soft | 有开始时间基准的业务单 | ⌈绝对开始时间偏差 / 60⌉ | 新增计划稳定性 |

### 实时重排扩展（2026-10-31）

`Problem.execution` 非空时使用执行上下文；缺省保持静态语义。路线的显式 `initial_load` 是实际出发载荷，不再从剩余配送单推断。取件/装货增加、配送/卸货减少载荷；任何站点出现负载荷计 `execution_consistency`。装卸辅助站不计业务单数和未派罚分，仍计行驶、服务、容量和可达性。

路线累计业务单数 = 已完成单数 + 剩余业务单数；均衡单数和日上限使用累计值。工作时间均衡使用已工作秒数 + 剩余行驶/服务秒数；配送比例均衡使用已配送量 + 剩余配送需求。已支付固定费用不再次计费。已有完成单、实际工作、已支付固定费用、实际非零载荷或必须回程的空路线仍计累计量和回程；不可用车辆不假定能自行回程。未使用静态空路线保持零分。

新增三项见表 22–24。物理一致性包括负载荷、装货前配送/不同车装卸及承诺时间范围；执行上下文中不允许关闭或改变物理一致性权重。已完成/服务中前置的时间和车辆比较仍计入第 5、6 项，关联比较计入第 10 项，沿用各自权重而不偷偷升级为物理硬约束。采用方案独立核查物理一致性、容量、可达性的匹配量为零，即使容量或可达性评分权重被关闭也不能采用违规计划。

车辆不可用、工单禁止派入、实物归属车辆和锁定路线前缀是结构限制，不通过调权重解除。锁前缀中的工单必须固定指派；初始解与所有移动都检查前缀。行驶中的下一站不自动上锁，服务中任务由投影层保留为执行事实并推迟该车后缀的可用时刻。已完成前置依赖以实际结束时间、实际车辆作为固定边界参与剩余工单评分。已完成历史本身不再计迟到和未派罚分。

同一执行上下文内比较候选与基准，历史累计量及实际载荷必须相同。所有新增匹配量同时进入全量 oracle、增量评分和数值上界检查。

**依赖执行检查（2026-11-02）。** 采用方案还要求时序依赖的匹配量为零，不允许通过人工接受其他业务违约绕过前置要求。开始服务时，全部前置必须已有不晚于事件时刻的实际完成记录；同一原子事件中同时报告前置完成也可满足条件。服务中/完成后的工单依赖作为事实边界不可修改。关闭搜索权重不允许提前执行后续工单。

## 3. 编译期剪枝

编译时仅对 `pinned` 工单把候选集合限制为原 agent；其他工单保留全部 agent。技能、资质、网点和日期标记可用于安排候选探索顺序，不能据此永久删除候选。

Hard 罚分也有权重和总量比较，未派可能触发改派罚分；取消一站还可能改变其他工单的到达时间或暴露不可达边。因此不能只依据 `ticket_unassigned` 是否带 hard 分量，就保证取消指派优于带硬约束违反的指派。日期评分以实际开始服务日为准，也不能用 agent 的静态日字段代替。

没有 agent 时，非固定工单可以未派。`UnassignedTicket.reasons` 区分 `no_eligible_agent` 与搜索结果中的 `not_assigned`；不把未经证明的静态剪枝原因当作无法指派的证据。

## 4. 从 v1 导入

`vrp import-v1` 把 vrp-0 的 JSON 转成 v2 请求，只用于基准测试和历史回放，不是兼容模式。

| v1 | v2 |
|---|---|
| `agent_is_virtual` | `ticket_unassigned` |
| `ticket_arrival_time_match_expected`（旧名） | `ticket_start_service_time_match_expected` |
| `agent_qualification_level_match_ticket`（旧名，整数等级） | `agent_qualification_levels_match_ticket`，整数等级映射为资质键 `__level` |
| `agent_brands_contain_ticket_brand`、`agent_product_lines_match_ticket`（旧名） | 不支持；数据里确有品牌或产品线要求时给出警告 |
| 虚拟 agent | 丢弃，其上的工单视为未派 |
| `weight`、`vol`（浮点） | 容量维 `weight`、`volume`，乘 1000 取整，`penalty_unit` = 1000，即仍按 v1 单位计罚 |
| `max_overload_ratio` r | 重量维容差 = round((r − 1) × 1000)；r ≤ 1（含数据里的 0，表示未设置）时为 0 |
| `shift_off_time` | `end_time`。缺失时不设；早于 `shift_start_time` 时（v1 结果文件改写过开始时刻）也不设，并给出警告 |
| `min_start_time` | `earliest_start`；其所在日为计划日 |
| `max_end_time` | 同时作为 `latest_start` 和 `latest_end`（v1 两条约束各取一种解释） |
| `duration` 缺失 | 按 v1 `estimateDuration` 估算 |
| `ref_tickets` | 下游引用，与对应 `dep_tickets` 合并为同一有向依赖；只有原配置显式启用 `relation_tickets_same_agent` 时才另外保留独立同资源关联 |
| `dep_tickets` | 前置列表，转换为 `precedences`（前置工单在前）；与下游引用去重，默认启用时序要求 |
| `original_agent` | 只要有任意工单带这个字段，就按字段取值；否则用输入里的当前指派 |
| `time_restricted_traffic_charge` | `restricted_charge_per_hour` |
| 矩阵中的 `Long.MAX_VALUE` 和缺失项 | 未知边，由 fallback 补齐 |
| POI 坐标 | 优先取 `entr_loc`，否则取 `location` 文本（"经度,纬度"）。v1 的 `loc.lat` 存的其实是经度，导入时纠正 |

v1 没有的约束（`unreachable_leg`、`agent_shift_overtime`）按 v2 默认权重计；`estimate_buffer` 不设，即使用默认余量。

**默认 fallback。** 曼哈顿距离，经度按场景所在纬度换算；绕行系数 1000‰；车速分段：
- 30 km 以上：60 km/h；
- 20 km 以上：45 km/h；
- 10 km 以上：30 km/h；
- 5 km 以上：20 km/h；
- 其余：15 km/h。

v1 的 `TransitCalculator` 本意如此，但它拿米和公里阈值比较，实际上 20 米到 30 公里都按 45 km/h 估算。

## 5. 待确认事项

- **品牌和产品线约束。** 是否恢复为通用的"技能组"约束。

**已决定、不在内核实现：AUX 时间窗放宽。** v1 对 AUX 场景已指派工单的时间窗放宽（`RoutePlan.updateMinStartTimeAndMaxEndTime` / `rearrange`）予以保留。需要放宽时间窗时，由调用方在构造请求时显式设置并记录变更。v2 内核不改写输入。
