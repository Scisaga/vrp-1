# 场景化 NP 难与 PSPACE 问题：启发式算法与算子

本文将 VRP 之外的组合优化、自动行动规划、逻辑推理与验证问题放在同一张问题地图中，说明实际场景、解的表示、常用搜索框架和领域算子。文献及日期核对于 **2026-11-03**。本文是研究概览，所列问题不是 vrp-1 已支持功能；项目当前实现见 [search.md](search.md)，VRP 扩展候选见 [search-roadmap.md](search-roadmap.md)。

场景名称只说明一种建模方向。现实业务采用什么约束、输入编码和目标，会影响复杂性及适用算法；表中的领域算子是代表性做法，不表示引用的某一篇论文覆盖了整行的全部业务变体。

## 1. 先区分复杂性、搜索框架与算子

**NP、NP 难和 PSPACE 不是同一种标签。** NP 是决策问题的复杂性类：肯定答案存在多项式长度、可在多项式时间验证的证书。生产排程、装箱、选址等通常讨论的是 NP 难优化问题，其常见阈值决策版本是 NP 完全问题。NP 难本身不意味着问题属于 NP；下面的优化表只讨论通常具有多项式规模显式方案的模型。

**PSPACE 是可用多项式空间判定的问题类，且 NP ⊆ PSPACE，目前不知道二者是否相等。** 本文单列的是典型 PSPACE 完全问题，不是说它与 NP 没有交集。此类问题常见于紧凑表示的状态空间、没有预设多项式长度上限的行动序列，以及层数随输入增长的“我选择—环境选择—我再选择”。这些现象提供建模线索，不能代替复杂性证明。

**不是所有调度、分配、图问题都 NP 难。** 固定图上的普通最短路、最小生成树、线性二分指派、2-SAT 和单纯 DAG 拓扑排序都有多项式算法。有限容量、多资源耦合、冲突、选择子集、先后与状态效果的组合，才可能形成表中较难的一般模型。应先检查是否能归入可高效求解的特例。

| 层次 | 要解决的问题 | 例子 |
|---|---|---|
| 建模与表示 | 一个候选解或状态是什么 | 工序排列、人员 × 日期班表、选中集合、布局排列、布尔赋值、行动序列、应对策略 |
| 构造 / 解码 | 怎么得到第一个方案或把编码转为可执行方案 | 贪心插入、优先级规则、Serial Schedule Generation Scheme（串行排程生成）、装箱放置规则 |
| 邻域 / 动作算子 | 下一步具体改变什么 | 交换两工序、搬移任务、翻转变量、互换颜色、打开 / 关闭设施；行动规划中则是带前置条件和效果的动作 |
| 搜索框架 | 怎样选择、组合和接受这些改变 | Tabu、模拟退火、VNS、LNS / ALNS、遗传局部搜索、启发式前向搜索、MCTS |
| 验证与证明 | 怎样确认可行、最优、无解或必胜 | 独立校验器、上下界、完整分支搜索、SAT / QBF 证书、模型检查 |

“启发式”既可以指没有最优保证的优化方法，也可以只是完整求解器内部的分支或遍历顺序。使用启发函数的 A* 在满足相应条件时可以求最优；CDCL、QCDCL 或分支定界也可以使用启发式，同时保留证明能力。不能把它们一律称为近似算法。

## 2. 常见 NP 难优化场景

以下讨论的是各模型的一般形式，不表示每个特例或每个实例都同样难。

| 实际场景 / 问题 | 主要决策与解的表示 | 常用启发式算法 | 典型领域算子与注意点 |
|---|---|---|---|
| **工厂作业车间 / 柔性作业车间（JSSP / FJSP）** | 每道工序在哪台机器做、同机工序按什么顺序做；表示为机器上的工序序列及可选机器分配 | 派工规则构造、关键路径局部搜索、Tabu、ILS / VNS、LNS | 关键块边界附近相邻交换（N5 等）、工序插入；柔性模型还可换机器。重排不得引入工艺 / 机器顺序环，不能只交换开始时间数值。[D01](#d01) |
| **工程项目 / 研发 / 检修项目（RCPSP）** | 有先后关系的活动怎样争用有限人员、设备和预算；常用活动优先级列表再解码为时间表 | 优先级多起点、Serial SGS、遗传局部搜索、VNS、LNS | 在满足先后关系的范围内插入 / 交换活动，移除瓶颈活动后重排；多模式变体还可换执行模式。时间应由资源和先后关系重新解码。[D02](#d02) |
| **护士 / 客服 / 运维人员排班** | 谁在哪天上哪个班，覆盖需求、资格、休息与公平性；表示为人员 × 日期的班表 | 启发式排序、VNS、Tabu、LNS / 约束修复 | 换一个班、两人换班、交换连续多日班段、循环轮换、移除高冲突人员后修复；技能覆盖、连续工作和休息约束需一起检查。[D03](#d03) |
| **排课 / 考试 / 频率分配（图着色及扩展）** | 冲突任务不能放同一时段 / 频段；基础表示为顶点的颜色，实际排课另有教室和容量 | DSATUR 贪心构造、最小冲突、Tabu、颜色类消除、LNS | 单点换色、两点换色、Kempe chain：互换两种颜色构成的连通分量中的颜色。它对基础正确着色的性质不能自动推广到教室、容量等附加约束。[D04](#d04) |
| **装箱 / 装柜 / 板材切割 / 图片图集排布** | 物品放进哪些箱子、放置顺序、位置和方向；不同维度有不同表示 | 一维 First Fit / Best Fit Decreasing；二维 Skyline、MaxRects、Best Fit；外层用顺序搜索、退火或 LNS | 换箱、交换物品、改变放置顺序、允许时旋转 90°、清空一个箱后重装。几何不重叠、承重和必须贯穿切割等约束需各自建模；一维规则不能直接当作三维方案。[D05](#d05) |
| **预算内项目选择 / 多资源投资组合（多维 0-1 背包）** | 选择哪些不可分项目，在多个预算限制下取得收益；表示为 0-1 选择向量 | 收益 / 资源启发式、GA / Memetic、Tabu、LNS、整数规划邻域修复 | 加入 / 删除项目、1↔1 或 1↔多项目交换、删去超预算项目后回填；交叉后需要容量修复。[D06](#d06) |
| **仓库 / 服务站选址、传感器覆盖、测试集选择** | 打开哪些设施并分配需求，或选哪些集合覆盖全部要求 | 增益 / 成本贪心、设施局部搜索、VNS、LNS | 设施 add / drop / swap、多设施交换、重新分配客户；固定 k 的选址需保持数量，集合覆盖需在删集合后修复缺失覆盖。选址与集合覆盖是相关但不同的模型。[D07](#d07)、[D08](#d08) |
| **云任务 / GPU / 员工任务分配（GAP 等）** | 每个任务分给哪个容量有限的资源，代价和资源消耗随分配变化 | 贪心 + 修复、Tabu、ejection chain、LNS | 单任务换资源、两任务互换、循环重分配、插入难任务时连锁弹出其他任务。简单线性一对一指派是多项式问题，这里指存在容量和消耗耦合的一般化分配。[D09](#d09) |
| **车间 / 医院 / 芯片模块布局（QAP）** | 将设施或模块放到位置，降低流量 × 距离等成对成本；表示为位置排列 | Robust Tabu、模拟退火、ILS、遗传局部搜索 | 交换两个位置、三位置循环、成组搬移 / 重排。一次交换会影响与许多其他模块的成对成本，不能按路径的两条边差值评估。[D10](#d10) |
| **芯片电路划分 / 分布式数据分区** | 把图或超图的点分到容量平衡的分区，减少跨区边 / 超边 | Kernighan–Lin、Fiduccia–Mattheyses（FM）、多层粗化与细化、局部搜索 | 按 gain 搬移一个点、成对交换、解锁一批点并回退到最好前缀、粗化后再细化。均衡 / 容量要求是难度来源之一，不能与普通最小割混为一谈。[D11](#d11) |
| **软件配置 / 逻辑规则一致性 / 加权冲突最小化（SAT / MaxSAT）** | 给变量赋真假，满足子句或尽量减少加权违约；表示为布尔向量 | GSAT / WalkSAT、随机游走、子句加权、重启；需要证明时使用 CDCL / MaxSAT 完整求解 | 单变量 flip、多变量 flip、从未满足子句中选变量、噪声扰动。局部搜索找到满足赋值可验证可行；搜索失败不能据此宣布不可满足。[D12](#d12) |

### 三个典型领域算子的直观例子

**关键块交换。** 生产完工时间常由某条关键路径决定。若只改动具有大量空闲时间的工序，目标可能几乎不变；JSSP 邻域因此优先改变关键路径上同机连续工序块的边界顺序，再重新计算可行时间表。[D01](#d01)

**Kempe chain。** 排课的图着色简化模型中，如果 A 课想从周一改到周二，单独换色可能撞课。可以找出与 A 连通、且当前只使用这两种颜色的冲突分量，把整个分量的两种颜色互换。这是按冲突结构联合移动，不是随意多换几个时段；完整教室和容量模型仍要独立检查。

**Ejection chain。** 云资源分配中，任务 A 想搬到机器 2，但容量不够；可把 B 从机器 2 搬到机器 3，再把 C 搬到其他机器，最终容纳 A。一个连锁移动能跨过单点搬移都不可行的障碍，类似 VRP 中的联合改派，但资源消耗与代价来自该领域模型。[D09](#d09)

## 3. 典型 PSPACE 完全问题与场景映射

PSPACE 完全结论依赖下表中的正式问题和输入条件。工程场景是否达到这些表达能力，需要另行建模和证明，不能仅凭“有状态”“有对抗”就贴标签。

| 正式问题与工程场景映射 | 为什么与静态分配不同 | 常用算法与启发信息 | 算子 / 转移与复杂性边界 |
|---|---|---|---|
| **命题 STRIPS 计划存在性**；设备操作、部署、装配、故障恢复的自动行动规划 | 需要选择行动顺序；动作有前置条件、增加效果和删除效果，可使先前成立的条件失效，计划长度不预先限制为多项式 | 启发式前向搜索、FF 的 delete relaxation、LAMA 的 landmarks、贪心最佳优先 / 加权 A*、重复状态检测 | 应用一个合法动作、按状态估计目标距离、优先扩展有用动作；宏动作和计划片段替换需保留真实状态效果。一般命题 STRIPS 计划存在性 PSPACE 完全；若只是对既定工序 DAG 排时间，更接近 RCPSP。[D13](#d13)–[D15](#d15) |
| **一般 QBF 真值判定**；多阶段对抗配置、环境故障下的应对策略 | 存在量词是我方选择，全称量词是环境选择；要寻找依赖于允许观察信息的应对，而不是只找一组布尔值 | QDPLL / QCDCL、依赖感知分支、clause / cube learning、重启；基于展开和 CEGAR 的方法 | 分支赋值、传播、回跳、学习约束、增加反例实例、扩展量词。PSPACE 完全指量词交替层数不受常数限制的一般 QBF；固定层数的变体不能统一按此结论处理。[D16](#d16)、[D17](#d17) |
| **LTL 满足性 / 一般 LTL 模型检查**；并发协议、自动流程的安全性与活性验证 | 关心所有可能执行是否满足时序性质，反例可能是有限前缀加无限重复循环，而非某张静态安排表 | 自动机乘积与按需状态遍历、符号表示、带启发顺序的反例搜索；有界检查可用 SAT 编码 | 扩展后继状态、检查可接受循环、相同状态去重、在正确性条件下做偏序 / 对称规约。一般 LTL 满足性及模型检查的联合复杂性 PSPACE 完全；不能把“公式固定”或“一次固定界限的检查”直接归为同一难度。[D18](#d18) |
| **多项式步数上限内的特定完全信息对抗博弈**；策略游戏和竞争资源选择的形式模型 | 我方的一步好选择必须经得起对方后续响应，解通常是条件策略，不是一条只对固定对手有效的行动序列 | Minimax、Alpha–Beta、迭代加深、启发式局面估值、置换表；预算有限时可用 MCTS / UCT | 生成合法落子 / 动作、动作排序、树展开、模拟和回传。Node Kayles 等特定广义游戏具有 PSPACE 完全胜负判定；棋类规则和步数限制不同，不能把所有游戏统一称为 PSPACE 完全。[D19](#d19)、[D20](#d20) |

### 自动行动规划：delete relaxation 与 landmarks

以部署流程为例，某个动作要求“数据库已锁定”，另一个动作完成后会解除锁；后者会破坏前者需要的事实。FF 的启发式计算先忽略动作删除事实的效果，在这个放松问题上估计还需要哪些动作、哪些动作值得优先尝试，实际执行候选时仍使用真实前置条件和效果。放松计划的成本估计不自动等于原问题的最优距离。[D14](#d14)

Landmark 是所有有效计划都必须经过的事实或条件。例如某类部署必须完成模式迁移；未达到的 landmark 可以帮助评估进展并选择搜索方向。LAMA 将 landmark 信息与 FF 类启发式及 anytime 搜索结合，持续寻找更低成本的有效计划。[D15](#d15)

### QBF：不是将 SAT 的 flip 算子原样套用

`∃ 初始配置 ∀ 故障 ∃ 修复动作 …` 表示修复可以响应已经发生的故障，但不允许偷看之后的环境选择。只有找到一组在某个故障下表现良好的配置，还不足以证明公式为真。依赖感知分支需要遵守量词信息限制；CEGAR 则可不断找出当前候选应对不了的环境反例，并据此精化问题。[D16](#d16)、[D17](#d17)

### 验证与博弈：搜索结果的含义要分开

LTL 检查找到一条经验证的真实反例，就能推翻相应的普遍性质；有限预算内没有找到反例通常只能报告未知，完整性论证或证明过程需另行成立。MCTS 的短时间高胜率也不等于已经证明必胜。启发式可以改变访问状态和动作的顺序，但不能用估分替代逻辑结论。

## 4. 跨场景可复用的搜索框架

| 框架 | 核心机制 | 常见适配对象 |
|---|---|---|
| **贪心 / 随机化贪心 / GRASP** | 按边际收益、紧迫性等指标逐步构造；随机化候选选择，再进行局部改进 | 覆盖、项目选择、选址、排程初解 |
| **局部搜索 / 最小冲突** | 每次选一个或少量修改，减少成本或违约 | SAT、着色、排班、布局；成败很依赖领域邻域 |
| **模拟退火 / Late Acceptance** | 用温度概率或历史分数门槛接受部分退步，离开局部最优 | 有可快速评估邻域的排班、布局、装箱等；[LAHC 来源](search.md#s07) |
| **Tabu Search / 禁忌搜索** | 用短期记忆避免立刻撤销移动，配合破禁和长期多样化 | 车间排程、QAP、一般化分配；不要求邻域全部改善 |
| **ILS / VNS / VND** | ILS 在局部最优之间扰动重启；VNS 系统改变邻域并扰动，VND 切换局部下降邻域 | 顺序、分配、时间表等组合结构；VNS 参考 [D21](#d21) |
| **LNS / ALNS** | 移除一批决策再修复；ALNS 根据历史表现调整移除 / 修复算子的选择 | 多约束排班、排程、装箱、分配；[ALNS 来源](search-roadmap.md#r05) |
| **GA / Memetic / ACO** | GA 重组多个解，Memetic 结合局部改进，ACO 通过累计构造信息引导后续构造 | 需要适合领域的编码、交叉、解码与修复；多维背包示例见 [D06](#d06) |
| **Matheuristic / Fix-and-Optimize / Local Branching** | 固定大部分变量，在选定邻域中用 MIP / CP 等方法优化和修复 | 硬约束多、手写修复复杂的排程、预算选择、分配；Local Branching 参考 [D22](#d22) |
| **贪心最佳优先 / A\* / 加权 A\* / Beam / IDA\*** | 用目标距离估计决定扩展顺序，按所选策略控制时间、内存和质量 | 自动行动规划、状态空间搜索。最佳优先、Beam 和加权搜索的保证与 A* 的适用条件不同 |
| **MCTS / UCT** | 在树中平衡探索与利用，反复模拟并回传结果 | 分支多、可模拟的序贯决策或游戏；抽样方法不自动证明必胜或满足所有环境分支。[D20](#d20) |
| **CDCL / QCDCL / CEGAR 的启发式引导** | 用分支排序、传播、学习、回跳、重启或反例精化控制完整推理 | 需要可满足 / 不可满足、真 / 假结论及证书的 SAT / QBF / 验证；这些是完整推理方法，不是普通成本下降算子 |

表中的算法属于不同层次，可以组合，但不能简单把名字叠加就认为更强。例如“VNS + 排班连续班段交换 + 约束修复”是可检验的组合；“使用遗传算法”尚未说明班表如何编码、交叉后如何满足休息要求，也就还不是完整方案。

## 5. 选算法时先找哪种结构

| 最突出的结构 | 优先考虑的表示与算子 | 下一步需要确认 |
|---|---|---|
| 排列与顺序 | swap、insert、block move、cycle；关键路径 / 关键块邻域 | 哪些重排保持先后关系，哪些变化能触及目标的瓶颈 |
| 人 / 机器 / 箱子分配 | relocate、swap、ejection chain、成组重分配 | 容量、资格、已承诺归属；链式移动是否必须原子提交 |
| 子集选择 | add、drop、1↔多交换、覆盖修复 | 预算、互斥、覆盖和收益耦合；能否用松弛提供界限 |
| 图冲突 | recolor、Kempe chain、gain-based vertex move | 基础图关系之外是否还有容量、房间或时序条件 |
| 几何放置 | 放置顺序、旋转、局部压紧、清空区域后重装 | 旋转是否允许，是否要求贯穿切割、支撑或不可重叠 |
| 行动改变事实 | 合法动作扩展、放松计划、landmark、重复状态检测 | 前置条件、增加 / 删除效果、是否有循环和不可逆动作 |
| 环境 / 对手交替选择 | AND / OR 分支、依赖感知赋值、反例精化、对抗树搜索 | 决策者何时能观察哪些信息；目标是高期望收益还是对所有响应成立 |
| 需要“对所有执行成立”的证明 | 完整验证流程及正确的规约，在其中使用启发式遍历 | 反例有效性、搜索完整性、停止时应报告证明还是未知 |

对于当前 VRP 项目，较接近的研究方向是带资源和前置关系的排程、人员排班、一般化分配。可以借鉴增量评估、撤销、候选引导、时间预算、取消和热启动的工程方法；各领域仍需要自己的状态、约束和独立校验器。上述邻近性属于工程判断，不是现有内核已经能够直接求解这些问题的结论。

跨场景比较应固定预算、实例分布和随机种子，并区分三类目标：找一个可行解、改善目标值、证明最优 / 无解 / 必胜。优化实验可报告可行率、目标值、界限差距与耗时；逻辑和验证实验还应报告已证明实例数、证书 / 反例检查结果和超时未知数。单一“分数更高”不能覆盖这些不同任务。

## 6. 代表文献与发布年月

下面是与上文对应的原始方法、实验研究或复杂性论文，不是完整的文献史。日期对应明确引用的版本；原刊、会议、技术报告与回溯上线日期分开说明，不能仅按 DOI 中的年份推断发表时间。

1. <a id="d01"></a> **D01 — Eugeniusz Nowicki, Czesław Smutnicki.** [A Fast Taboo Search Algorithm for the Job Shop Problem](https://doi.org/10.1287/mnsc.42.6.797). *Management Science*, 42(6), 797–813。**1996-06**，出版社日期 1996-06-01；关键路径与工序块邻域的代表性方法。
2. <a id="d02"></a> **D02 — Rainer Kolisch, Sönke Hartmann.** [Experimental Investigation of Heuristics for Resource-Constrained Project Scheduling: An Update](https://doi.org/10.1016/j.ejor.2005.01.065). *European Journal of Operational Research*, 174(1), 23–37。**2006-10**，正式卷期日期 2006-10-01；包含作者的统一计算实验与启发式组件比较。
3. <a id="d03"></a> **D03 — Edmund K. Burke, Timothy Curtois, Gerhard Post, Rong Qu, Bart Veltman.** [A Hybrid Heuristic Ordering and Variable Neighbourhood Search for the Nurse Rostering Problem](https://doi.org/10.1016/j.ejor.2007.04.030). *European Journal of Operational Research*, 188(2), 330–341。**2008-07**，正式卷期日期 2008-07-16。
4. <a id="d04"></a> **D04 — Daniel Brélaz.** [New Methods to Color the Vertices of a Graph](https://doi.org/10.1145/359094.359101). *Communications of the ACM*, 22(4), 251–256。**1979-04**；DSATUR 的经典来源，此处不将 Kempe chain 的历史归属也归给该论文。
5. <a id="d05"></a> **D05 — Jukka Jylänki.** [A Thousand Ways to Pack the Bin — A Practical Approach to Two-Dimensional Rectangle Bin Packing](https://raw.githubusercontent.com/juj/RectangleBinPack/master/RectangleBinPack.pdf). 作者技术文稿，封面 **2010-02**（2010-02-27）；比较二维矩形装箱的 Skyline、MaxRects 等工程方法，不作为一维 FFD 的首创文献。
6. <a id="d06"></a> **D06 — P. C. Chu, J. E. Beasley.** [A Genetic Algorithm for the Multidimensional Knapsack Problem](https://doi.org/10.1023/A:1009642405419). *Journal of Heuristics*, 4, 63–86。**1998-06**，Springer 标为 June 1998；结合问题知识的启发式算子与遗传搜索。
7. <a id="d07"></a> **D07 — Vijay Arya, Naveen Garg, Rohit Khandekar, Adam Meyerson, Kamesh Munagala, Vinayaka Pandit.** [Local Search Heuristics for k-Median and Facility Location Problems](https://doi.org/10.1137/S0097539702416402). *SIAM Journal on Computing*, 33(3), 544–562。**2004-03**，依据[作者论文 PDF](https://www.cse.iitd.ac.in/~pandit/lsearchSICOMP.pdf)首页的 published electronically 2004-03-23；现网站的回溯上线日期不替代该日期。文中的近似界依赖其度量、容量等具体假设，不能直接外推到全部选址业务。
8. <a id="d08"></a> **D08 — Václav Chvátal.** [A Greedy Heuristic for the Set-Covering Problem](https://doi.org/10.1287/moor.4.3.233). *Mathematics of Operations Research*, 4(3), 233–235。**1979-08**，出版社日期 1979-08-01。
9. <a id="d09"></a> **D09 — Mutsunori Yagiura, Toshihide Ibaraki, Fred Glover.** [An Ejection Chain Approach for the Generalized Assignment Problem](https://doi.org/10.1287/ijoc.1030.0036). *INFORMS Journal on Computing*, 16(2), 133–151。**2004-05**，出版社日期 2004-05-01，卷期标为 Spring 2004。
10. <a id="d10"></a> **D10 — Éric Taillard.** [Robust Taboo Search for the Quadratic Assignment Problem](https://doi.org/10.1016/S0167-8191(05)80147-4). *Parallel Computing*, 17(4–5), 443–455。**1991-07**，正式卷期为 July 1991；DOI 中的 `(05)` 不是论文发表年份。
11. <a id="d11"></a> **D11 — Charles M. Fiduccia, Robert M. Mattheyses.** [A Linear-Time Heuristic for Improving Network Partitions](https://doi.org/10.1109/DAC.1982.1585498). *19th Design Automation Conference*, 175–181。**1982-06**（会议 1982-06-14～16）；基于单点搬移与 gain 更新的划分方法，IEEE 资料库的 2006 年收录时间不是原始发表时间。
12. <a id="d12"></a> **D12 — Bart Selman, Henry A. Kautz, Bram Cohen.** [Noise Strategies for Improving Local Search](https://www.cs.ubc.ca/~hoos/SATLIB/ubcsat/algorithms/SelKauCoh94.pdf). *AAAI-94*, 337–343。**1994-07**，论文页首注明 Seattle, July 1994；随机局部搜索、混合随机游走及 SAT / MaxSAT 应用。
13. <a id="d13"></a> **D13 — Tom Bylander.** [The Computational Complexity of Propositional STRIPS Planning](https://doi.org/10.1016/0004-3702(94)90081-7). *Artificial Intelligence*, 69(1–2), 165–204。**1994-09**，正式卷期为 September 1994；一般命题 STRIPS 计划存在性与受限子类的复杂性来源。
14. <a id="d14"></a> **D14 — Jörg Hoffmann, Bernhard Nebel.** [The FF Planning System: Fast Plan Generation Through Heuristic Search](https://doi.org/10.1613/jair.855). *Journal of Artificial Intelligence Research*, 14, 253–302。**2001-05**，期刊日期 2001-05-01；delete relaxation、启发式信息与搜索策略。
15. <a id="d15"></a> **D15 — Silvia Richter, Matthias Westphal.** [The LAMA Planner: Guiding Cost-Based Anytime Planning with Landmarks](https://doi.org/10.1613/jair.2972). *Journal of Artificial Intelligence Research*, 39, 127–177。**2010-09**，[作者论文 PDF](https://gki.informatik.uni-freiburg.de/papers/richter-westphal-jair10.pdf)首页标为 published 09/10。
16. <a id="d16"></a> **D16 — Florian Lonsing, Armin Biere.** [DepQBF: A Dependency-Aware QBF Solver — System Description](https://doi.org/10.3233/SAT190077). *Journal on Satisfiability, Boolean Modeling and Computation*, 7, 71–76。**2010-08**，[期刊论文 PDF](https://journals.sagepub.com/doi/pdf/10.3233/SAT190077)首页标为 published August 2010；与源码首版 **2010-03** 的日期分开。[作者项目页](https://fmv.jku.at/depqbf/)说明传播、clause / cube learning 等实现背景。
17. <a id="d17"></a> **D17 — Mikoláš Janota, William Klieber, João Marques-Silva, Edmund Clarke.** [Solving QBF with Counterexample Guided Refinement](https://doi.org/10.1016/j.artint.2016.01.004). *Artificial Intelligence*, 234, 1–25。**2016-05**，正式卷期为 May 2016；引用期刊扩展版，讨论量词展开、CEGAR 与 DPLL 学习的结合。
18. <a id="d18"></a> **D18 — A. Prasad Sistla, Edmund M. Clarke.** [The Complexity of Propositional Linear Temporal Logics](https://doi.org/10.1145/3828.3837). *Journal of the ACM*, 32(3), 733–749。**1985-07**，[作者保存的期刊 PDF](https://www.cs.cmu.edu/~emc/papers/Conference%20Papers/TheComplexityOfPropositionalLinearTemporalLogics.pdf)页脚列出 July 1985；与 1982 年会议前身分开。
19. <a id="d19"></a> **D19 — Thomas J. Schaefer.** [On the Complexity of Some Two-Person Perfect-Information Games](https://doi.org/10.1016/0022-0000(78)90045-4). *Journal of Computer and System Sciences*, 16(2), 185–225。**1978-04**，正式卷期为 April 1978；特定有限步完全信息游戏的 PSPACE 完全胜负判定背景，不是一般棋类复杂性的统一结论。
20. <a id="d20"></a> **D20 — Levente Kocsis, Csaba Szepesvári.** [Bandit Based Monte-Carlo Planning](https://doi.org/10.1007/11871842_29). *ECML 2006*, LNCS 4212, 282–293。**2006-09**（会议 2006-09-18～22）；UCT 的代表论文。原文对有限时域 / 折扣 MDP 的结论不能直接替代对任意对抗游戏的必胜证明。
21. <a id="d21"></a> **D21 — Nenad Mladenović, Pierre Hansen.** [Variable Neighborhood Search](https://doi.org/10.1016/S0305-0548(97)00031-2). *Computers & Operations Research*, 24(11), 1097–1100。**1997-11**，正式卷期为 November 1997。
22. <a id="d22"></a> **D22 — Matteo Fischetti, Andrea Lodi.** [Local Branching](https://doi.org/10.1007/s10107-003-0395-5). *Mathematical Programming*, 98, 23–47。**2003-09**，出版社日期 2003-09-01；在整数规划中定义局部邻域并进行搜索的代表方法，不将所有 Fix-and-Optimize 变体都归为同一算法。
