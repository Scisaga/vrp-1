# 待实现的 VRP 算子与算法

本文整理尚未实现的搜索扩展候选，包含来源论文、发布年月、适用场景与项目适配点。核对日期为 **2026-11-02**；已实现行为及其来源见 [search.md](search.md)，评分以 [constraints-v2.md](constraints-v2.md) 为准，执行边界见 [realtime-planning.md](realtime-planning.md)。

VRP 之外的排程、排班、装箱、选址、SAT、自动行动规划、QBF 与验证等问题，另见 [场景化 NP 难与 PSPACE 问题概览](combinatorial-search-landscape.md)。

“待实现”表示尚未作为相应的专门算子或完整算法实现，不表示这些能力已经可由参数启用，也不承诺全部实施。优先级和适用性是结合项目的工程判断，尚无本项目实测收益结论。基础随机移除 / 贪心修复、普通 Swap、Or-opt、线程并行和实时重排入口已经存在，下面说明各扩展在此基础上新增什么。

## 文献与日期口径

- 来源可以是原始论文、代表性算法论文或项目定制方案的背景参考；会明确区分，不给定制算子虚构唯一首创论文。
- 期刊优先记录正式卷期年月；首次在线发表和预印本时间在有必要时另列。会议论文、技术报告注明引用的版本。年月是文献的日期，不是本项目的交付时间。
- 论文中的成本函数、可行性假设和复杂度不自动适用于本项目的有向、多车型、三级业务评分、跨资源依赖和执行状态。

## 候选移动与修复算子

| 候选 | 相对当前实现的新增能力 | 适用场景与建议顺序 | 来源与发布年月 |
|---|---|---|---|
| **Regret-2 / Regret-3 插入** | 当前按随机工单顺序找各自最优插入；regret 修复优先处理失去当前选择后替代代价较大的工单 | **第一批**：紧时间窗、稀缺技能、难派工单；可同时用于初解和移除后的修复 | Ropke、Pisinger，[R05](#r05)，**2006-11** |
| **相关移除 / Shaw removal** | 按空间、时间窗、需求等关联选择一批工单，而非逐个独立随机移除 | **第一批**：同区域工单分散到多车、同一时间段拥挤；技能、网点等可作为项目的附加相关性特征 | Shaw，[R04](#r04)，**1998-10**（CP98 纸本论文集）；Ropke、Pisinger，[R05](#r05)，**2006-11** |
| **Worst removal / 高代价移除** | 用工单的移除收益或业务冲突贡献引导移除，仍保留随机性 | **第一批的补充**：高绕行、迟到、超载、班次冲突；移除收益须通过现有评分计算，不能只看几何距离 | Ropke、Pisinger，[R05](#r05)，**2006-11** |
| **2-opt\* / 路线尾段交换** | 两条路线各切一处并交换后缀，保持各段方向；当前片段反转和 k-opt 均不能替代这个跨路线邻域 | **第一批**：路线交叉、时间窗与工作量调整 | Potvin、Rousseau，[R01](#r01)，**1995-12** |
| **SWAP\*** | 两车各移除一单，交换车辆后各自寻找插入位置，允许改变位置而非原位互换 | **第一批试验**：容量、车型成本或时间窗使原位 swap 难以改善的情况 | Vidal，[R02](#r02)，**2022-04**（期刊版）；预印本 **2020-11** |
| **Cross-exchange / 小段交换** | 交换两条路线的连续片段，可先限于 2↔1、2↔2、3↔1；当前 Or-opt 只搬移一段，不同时交换另一段 | **第二批**：单点移动无法跨越容量、单量或业务分组障碍 | Taillard 等，[R03](#r03)，**1997-05** |
| **SISR / 相邻工单串移除与重建** | 按相邻路线移除连续工单串，形成空间与容量余量，再重建；完整方法还包含带 blinks 的插入及车队缩减过程 | **第二批**：密集配送、区域划分不佳；仅增加 string removal 时应称 SISR-inspired，不声称完整复现 SISR | Christiaens、Vanden Berghe，[R06](#r06)，**2020-03～04**（双月卷期）；首次在线 **2020-01** |
| **Ejection pool / 引导弹出与路线消除** | 为难插工单弹出其他工单，再继续安置；可尝试清空一条路线以减少用车 | **按业务目标启用**：满载难插单、成本较高的闲散车辆；不把最少车辆数偷偷变成新的首要目标 | Nagata、Bräysy，[R07](#r07)，**2009-09** |
| **取送对 / 装卸子行程联合移动** | 联合移除和重插必须同车的装货—配送对或完整子行程，保持内部先后关系；区别于可能只搬走一端的普通 relocate / Or-opt | **实时业务优先**：尚未执行的装卸、取送与补货。已经在车货物仍受实际归属约束，不能凭搜索自由换车 | PDPTW 的成对移除 / 重插参考 Ropke、Pisinger，[R05](#r05)，**2006-11**；本项目辅助访问与锁的适配属于定制设计 |

## 候选算法与配套改进

| 候选 | 新增能力 | 本项目的适配边界 | 来源与发布年月 |
|---|---|---|---|
| **ALNS / 自适应大邻域搜索** | 保留多种移除、修复方法，按历史表现调整使用频率 | 当前只有基础移除 / 修复和固定周期调度。可保留 Late Acceptance，先增加算子统计与选择机制，不必一并更换接受策略 | Ropke、Pisinger，[R05](#r05)，**2006-11** |
| **依赖感知联合邻域** | 联合调整选定前置、后续及受影响路线，减少中间方案因等待或时序违约被拒绝的情况 | 已有跨资源依赖推时与评分；尚无专门联合邻域。一般依赖只要求先后，不要求同车；任务集合和联合插入规则需单独设计 | 方法背景为 PDPTW 联合修复 [R05](#r05)，**2006-11**；推广到本项目多前置 DAG 属于定制方案，不是该论文已经定义的同名算子 |
| **事件驱动的局部修复** | 新增 / 取消、故障等事件后，优先在受影响车辆及相关工单集合内修复，再按预算扩展范围 | 当前已有事件触发、热启动与完整剩余问题重排；待补的是受影响集合选择和局部搜索预算。冻结前缀不变，跨资源下游等待仍须完整传播 | 实时车辆调度的背景参考 Gendreau 等，[R08](#r08)，**1999-11**；本项目事件与货物语义属于定制方案 |
| **路线片段摘要 / 拼接评估** | 预计算连续片段的时间、载荷等摘要，按若干片段拼接评估移动，降低扩大邻域后的评分成本 | 当前已有路线级增量评分，尚无常数时间片段汇总。先覆盖能够证明可组合的约束；跨路线依赖传播不据此宣称 O(1)，最终评分仍须一致 | Savelsbergh，[S02](search.md#s02)，**1992-05**；Vidal 等，[R10](#r10)，**2014-05** |
| **POPMUSIC 候选集构造** | 优化多个局部子路径，把所得边汇集成候选集，作为普通近邻 / alpha 的可选替代 | **研究候选**：先测候选预处理是否成为瓶颈；目标仍是 ≤1000 单、≤200 车，不因此引入超大场景分区 | Helsgaun，[R09](#r09)，**2018-07**（技术报告） |
| **HGS / 种群与路线交叉** | 维护多样化解的种群，使用交叉、局部改进及多样性管理产生新解 | **研究候选**：当前多线程只交流最佳方案，尚无种群与交叉。原生多车型、执行锁和业务评分需要相应解码 / 修复，不能直接视为 HGS-CVRP 复现 | HGS-CVRP 代表性实现为 Vidal，[R02](#r02)，**2022-04**；多属性框架参考 [R10](#r10)，**2014-05** |

## 建议的试验顺序

1. **先补基础能力**：Regret-2/3、相关移除 / worst removal、2-opt\*、SWAP\*，分别做单算子开关对照；实时重排业务可将取送 / 装卸联合移动提前。
2. **再扩展组合邻域**：小段 Cross-exchange、SISR-inspired 移除、依赖感知联合移动，并检查候选评估成本。
3. **按目标增加算法调度**：用 ALNS 选择已有移除 / 修复算子；只有业务目标支持减少车辆时，才评估路线消除和引导弹出。
4. **最后评估更大改动**：路线片段摘要、POPMUSIC 或 HGS。片段摘要也可以在性能剖析证明评分成为瓶颈时提前。

每个试验都使用相同的时间、线程和预处理预算，覆盖多个种子。分别记录 hard / medium / soft、未派单、行驶成本、使用车辆、改派 / 改序 / 时间变化和评估耗时；只有较高层级分数相同时，才单独比较较低层级或路程收益。参考场景至少包含容量紧、时间窗紧、异构车辆 / 技能、跨资源多前置依赖和带执行锁的实时输入。私有案例与含客户信息的结果仅留在 `corpus/`。

## 实现时必须保留的项目边界

- 所有移动继续使用原生路线表示和 hard → medium → soft 字典序评分；regret 排序、移除收益和接受策略都要明确三级分数的比较方式，不能把 hard 任意换算为行驶秒数。
- `pinned` 不可取消或改派，但可在原路线内重排；执行锁前缀、不可调度车辆、工单禁止派入和实际货物归属继续作为结构限制。
- “相关工单”与“同车任务”分开处理。一般 `precedences` 是跨资源 AND 依赖，不从先后关系推导同车；等待变化须传播到所有受影响的下游路线。
- 技能、日期、网点及近邻只可引导候选顺序，不能成为未经证明的永久硬剪枝。成对任务也不能绕过现有执行一致性、容量、可达性与采用检查。
- 新邻域的结构保持与增量评分都需通过独立 oracle 差分验证。评分语义如需变化，先改评分文档，再同时修改 oracle 和增量实现；新增搜索参数如需进入外部契约，按 `proto/` 的单一契约规则同步文档。
- 移除、修复、联合插入和分支搜索都应协作检查时间 / 取消，保留已记录的全局最佳方案；`vrp-core` 继续只依赖标准库、无 I/O、无 async、无全局状态。

## 参考文献与日期依据

1. <a id="r01"></a> **R01 — Jean-Yves Potvin, Jean-Marc Rousseau.** [An Exchange Heuristic for Routeing Problems with Time Windows](https://doi.org/10.1057/jors.1995.204). *Journal of the Operational Research Society*, 46, 1433–1446。**1995-12**，出版社日期 1995-12-01；2-opt\* 的来源。
2. <a id="r02"></a> **R02 — Thibaut Vidal.** [Hybrid Genetic Search for the CVRP: Open-Source Implementation and SWAP\* Neighborhood](https://doi.org/10.1016/j.cor.2021.105643). *Computers & Operations Research*, 140, 105643。**2022-04**，正式卷期为 April 2022；[arXiv:2012.10384](https://arxiv.org/abs/2012.10384)首次提交 **2020-11**（2020-11-23），修订于 **2021-10**。这里分别引用 SWAP\* 邻域和 HGS 框架，不把它们当作同一层次的概念。
3. <a id="r03"></a> **R03 — Éric Taillard, Philippe Badeau, Michel Gendreau, François Guertin, Jean-Yves Potvin.** [A Tabu Search Heuristic for the Vehicle Routing Problem with Soft Time Windows](https://doi.org/10.1287/trsc.31.2.170). *Transportation Science*, 31(2), 170–186。**1997-05**，出版社日期 1997-05-01；以两路线连续工单段交换构造邻域。
4. <a id="r04"></a> **R04 — Paul Shaw.** [Using Constraint Programming and Local Search Methods to Solve Vehicle Routing Problems](https://doi.org/10.1007/3-540-49481-2_30). *Principles and Practice of Constraint Programming — CP98*, LNCS 1520, 417–431。**1998-10**，依据[论文集纸本出版信息](https://link.springer.com/book/10.1007/3-540-49481-2)的 1998-10-09，会议举行于 1998-10-26～30；章节页面的后续在线日期与原纸本日期不同。原方法以相关工单移除配合约束树搜索重插，本项目若改用 regret 修复应标为组合适配。
5. <a id="r05"></a> **R05 — Stefan Ropke, David Pisinger.** [An Adaptive Large Neighborhood Search Heuristic for the Pickup and Delivery Problem with Time Windows](https://doi.org/10.1287/trsc.1050.0135). *Transportation Science*, 40(4), 455–472。**2006-11**，出版社日期 2006-11-01；相关 / 随机 / worst 移除、regret 修复、PDPTW 成对处理和自适应算子选择的主要参考。
6. <a id="r06"></a> **R06 — Jan Christiaens, Greet Vanden Berghe.** [Slack Induction by String Removals for Vehicle Routing Problems](https://doi.org/10.1287/trsc.2019.0914). *Transportation Science*, 54(2), 417–433。**2020-03～04**，保留正式双月卷期范围；首次在线 **2020-01**（2020-01-15）。包含 adjacent string removal、greedy insertion with blinks，以及面向最少用车目标的补充过程。
7. <a id="r07"></a> **R07 — Yuichi Nagata, Olli Bräysy.** [A Powerful Route Minimization Heuristic for the Vehicle Routing Problem with Time Windows](https://doi.org/10.1016/j.orl.2009.04.006). *Operations Research Letters*, 37(5), 333–338。**2009-09**，正式卷期为 September 2009；引导弹出池、插入和路线缩减的代表性方法，不能据此给所有 ejection chain 变体指定同一首创来源。
8. <a id="r08"></a> **R08 — Michel Gendreau, François Guertin, Jean-Yves Potvin, Éric Taillard.** [Parallel Tabu Search for Real-Time Vehicle Routing and Dispatching](https://doi.org/10.1287/trsc.33.4.381). *Transportation Science*, 33(4), 381–390。**1999-11**，出版社日期 1999-11-01；动态车辆调度的方法背景，不是本项目局部修复规则的完整规格。
9. <a id="r09"></a> **R09 — Keld Helsgaun.** [Using POPMUSIC for Candidate Set Generation in the Lin-Kernighan-Helsgaun TSP Solver](https://rucforsk.ruc.dk/ws/portalfiles/portal/62731502/POPMUSIC_REPORT.pdf). Roskilde University，技术报告，封面 **2018-07**；此处引用的是 POPMUSIC 用于 LKH 候选生成的版本，不是 POPMUSIC 框架的首次提出。
10. <a id="r10"></a> **R10 — Thibaut Vidal, Teodor Gabriel Crainic, Michel Gendreau, Christian Prins.** [A Unified Solution Framework for Multi-Attribute Vehicle Routing Problems](https://doi.org/10.1016/j.ejor.2013.09.045). *European Journal of Operational Research*, 234(3), 658–673。**2014-05**，正式卷期日期 2014-05-01；多属性 VRP 的组件化求解、片段拼接评估与 UHGS 的参考。
