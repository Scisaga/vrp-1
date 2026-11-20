# 搜索实现与 LKH 参考

当前内核支持单任务多线程、热启动、协作取消和共享最佳方案。外部 `SolveOptions.search_strategy` 可选 `NEAREST`（默认，核心名 `Candidate`）、`BASELINE`、`ALPHA`、`ALPHA_K_OPT`；CLI 对应 `--search nearest|baseline|alpha|alpha-kopt`。不同策略须按同一时间与线程预算比较，新增策略暂不替换默认值。

本文记录已经实现的搜索行为及其论文来源。待实现的算子、算法、适用场景与评估顺序单独维护在 [搜索扩展候选](search-roadmap.md) 中。

## 已实现

1. 使用给定初解，或以固定工单构造种子方案。打乱未派工单顺序，逐张枚举候选 agent 与插入位置，用业务分数选择严格改善的位置。
2. 每个线程持有自己的路线、增量评分状态、随机数和长度 128 的 Late Acceptance 历史。只读问题及近邻缓存共享；严格改善的最佳方案通过互斥锁交换。
3. 基础算子为派入/取消/relocate、swap、片段反转。Candidate 模式另加 Or-opt：连续 1～3 张工单保序移动，可在同路线内或跨路线执行。
4. Candidate 模式按实际用到的矩阵，为每张工单保留最多 16 个近邻。排序指标是去程与回程时长的较小值；80% 概率尝试候选引导，20% 直接全域采样，候选不适用时也回退。该指标只影响探索顺序，不改变有向矩阵或实际评分。
5. 每 2048 次搜索循环从全局最佳方案重启。Baseline 使用随机移除/贪心修复；Candidate 交替尝试双桥扰动与移除/修复。双桥把同路线的四段 `A|B|C|D` 接成 `A|D|C|B`，不反转段内方向。
6. 固定工单不能取消或改派，但可以在原路线内重排。所有算子都使用现有 hard/medium/soft 字典序评分；非对称边、等待、容量、班次与依赖通过受影响路线的重算处理。返回前由独立 oracle 校验分数并生成解释。

候选构造复杂度为 O(P×N²)，存储为 O(P×N×16)，P 是实际使用的矩阵数，N 是工单数。构造按行及每 256 个候选检查停止条件，受同一个求解预算约束。此处面向 ≤1000 单、≤200 车，不引入超大场景分区。

## 已实现特性的论文来源与发布年月

下表区分移动算子、扰动、候选构造和接受策略。来源表示算法思想或代表性实现，不表示完整复现论文，也不把通用算子的代表性文献当作唯一首创来源。年月对应所引用的论文版本，不是本项目实现时间；期刊使用出版社列出的正式发表年月，技术报告和学位论文单独标明版本。完整题名、作者、链接及日期说明见文末参考文献。

| 类别 | 已实现特性 | 来源与发布年月 | 本项目的实现范围 |
|---|---|---|---|
| 初解 / 修复 | 贪心最优位置插入 | Solomon，[S01](#s01)，**1987-04** | 插入式构造的代表性来源；本项目打乱工单顺序，枚举车辆与位置，以业务分数选严格改善位置，未复现 Solomon I1 的选单和成本公式 |
| 移动 | Relocate、派入 / 取消、Swap | Savelsbergh，[S02](#s02)，**1992-05** | 参考 VRPTW 的边交换邻域；支持同路线和跨路线移动。未派集合与取消操作是本项目原生表示的扩展 |
| 移动 | 路线内片段反转（2-opt 形态） | Croes，[S03](#s03)，**1958-12** | 反转连续片段；有向矩阵下重算内部有向边和业务约束，不套用对称 TSP 的两条边差值公式 |
| 移动 | Or-opt | Or，[S04](#s04)，**1976-06**（博士论文） | 连续 1～3 单保序移动，支持同路线和跨路线；`NEAREST`、`ALPHA`、`ALPHA_K_OPT` 启用 |
| 扰动 | Double-bridge / 双桥 | Martin、Otto、Felten，[S05](#s05)，**1991-06**（技术报告） | 同路线四段重接为 `A｜D｜C｜B`，保持段内方向；用于周期扰动，未复现原文完整的 Markov chain 搜索 |
| 扰动 / 修复 | 随机移除 + 贪心修复 | Schrimpf 等，[S06](#s06)，**2000-04** | Ruin & Recreate 思想的基础版本；没有完整 SISR，也没有自适应选择多种移除 / 修复算子的 ALNS |
| 接受策略 | Late Acceptance | Burke、Bykov，[S07](#s07)，**2017-04**（期刊版） | 长度 128 的分数历史；候选不差于当前分数或对应历史分数即可接受，另行保存全局最佳方案 |
| 候选引导 | 普通近邻候选 | Helsgaun，[S08](#s08)，**2000-10** | 参考候选集引导的搜索思路；本项目按双向时长较小值取最多 16 个近邻，80% 候选引导 / 20% 全域采样是工程配置 |
| 候选构造基础 | 最小 1-tree、节点罚值次梯度优化 | Held、Karp，[S09](#s09)，**1970-12**；[S10](#s10)，**1971-12** | 在对称代理图上求 1-tree 并优化罚值；仅引导搜索，不把代理图下界当作原生 VRP 业务目标的下界 |
| 候选引导 | Alpha-nearness | Helsgaun，[S08](#s08)，**2000-10** | 按给定罚值下的 1-tree 敏感度选择候选；`ALPHA`、`ALPHA_K_OPT` 启用，预处理受预算限制 |
| 换边 | 可变深度连续 k-opt | Lin、Kernighan，[S11](#s11)，**1973-04**；Helsgaun，[S12](#s12)，**2009-07** | 参考可变深度交替换边和一般 k-opt 子移动；作用于单条路线，按完整业务分数提交严格改善 |
| 换边 | 非连续换边与子环修补 | Helsgaun，[S12](#s12)，**2009-07**，§5.2 | 从多子环闭合图继续寻找交替环修补；总删边数默认上限 5，可设 2～12，并有候选 / 分支预算 |

基础移动、近邻候选与双桥见 [neighborhood.rs](../crates/vrp-core/src/search/neighborhood.rs)；插入、接受策略和扰动调度见 [search.rs](../crates/vrp-core/src/search.rs)；alpha、换边和修补分别见 [alpha.rs](../crates/vrp-core/src/search/alpha.rs)、[kopt.rs](../crates/vrp-core/src/search/kopt.rs)、[patch.rs](../crates/vrp-core/src/search/kopt/patch.rs)。热启动、线程间共享最佳方案、具体历史长度和重启周期属于本项目的工程实现，不另指定一篇“首创论文”。

## Alpha-nearness

`ALPHA` 与 `ALPHA_K_OPT` 在工单节点构成的对称代理图上计算 alpha。边权仍是 `min(duration(i,j), duration(j,i))`；它只用于引导候选，不是原始有向、多车型、带业务约束 VRP 的下界。

- 固定节点 0 为 1-tree 的特殊节点。在其余节点上用稠密 Prim 求最小生成树，再加入特殊节点的两条最便宜边。
- 用 Held–Karp 次梯度更新节点罚值 π，边权变为 `c(i,j)+π[i]+π[j]`。最多 64 轮，保存已找到的最好下界及对应 π；这不承诺求出最优的拉格朗日乘子。
- 对非特殊节点的边，`alpha(i,j)=cπ(i,j)-max_edge_on_MST_path(i,j)`；特殊节点已有的两条边 alpha 为 0，其他边与第二条最便宜特殊边比较。使用 O(N²) 路径最大边扫描计算所有 alpha，按 `(alpha, cπ, 节点下标)` 选择最多 16 个候选。
- 这是给定 π 下的完整 1-tree 敏感度计算，包含特殊节点、负的变换边权与相同边权情况。测试通过枚举小图的所有 1-tree，对每条强制边独立核对 alpha。

Alpha 预处理总预算为 `min(总时限/10, 500ms)`；没有总时限时为 500ms，按使用中的矩阵分配。先建立普通近邻候选，再在预算内升级；未完成的矩阵保留近邻/全域探索，避免耗尽整个求解预算。`SolveStats.alpha_profiles`、`ascent_iterations` 和 `preprocessing_ms` 报告实际完成数量和成本。Alpha 阶段使用每矩阵 O(N²) 临时空间，处理完成后释放。

## 可变深度 k-opt

`ALPHA_K_OPT` 每 128 次搜索循环尝试一次换边搜索：选择原路线的一条边开始，交替添加候选边、删除原路线边，并在每一深度尝试闭合。连续交换链的端点不重复；删除边与新增边不重叠。闭合后若恰为一个完整环，去掉虚拟 depot 得到路线；若形成多个子环，则按下节继续尝试修补。它不是连续执行若干个已接受的 2-opt。

每次闭合都按原始有向矩阵和完整业务约束评估两个方向。只有严格改善当前分数才提交；较差或形成子环的中间闭合仍可向更深层探索，不按几何距离正增益剪枝。固定工单始终保留在原 agent。

`k_opt_depth` 限制一次提交中删除的原路线边总数，包含子环修补，0 使用 5，非零接受 2～12；实际总数也不超过路线加虚拟 depot 的边数。每次尝试至多 512 个连续搜索分支、另有总计 256 个修补分支，后者在本次尝试的所有闭合间共享。每个端点优先取 8 个路线内候选，工单节点另可连接虚拟 depot。每个分支检查时间/取消。`k_opt_improvements` 记录全部严格改善工作方案的次数。

当前 k-opt 作用于单条路线；跨路线仍由 relocate、swap、Or-opt 和移除/修复完成。尚未实现 POPMUSIC、完整受约束 TSP 变换和常数时间片段汇总。

## 非连续换边与子环修补

实现参考 Helsgaun 2009 §5.2 的 cycle patching 思路：连续交换的闭合可能形成若干不相连的环，另找交替删边/加边的环把它们连接起来，得到非连续交换。代码为独立 Rust 实现，没有拷贝或链接 LKH 源码。

1. 对临时闭合图按节点标记连通分量、统计大小，从最小子环选择一条仍保留的原路线边。
2. 删除该边，沿候选连接到尚未访问的另一个子环，再删除其中一条仍保留的原路线边。一个交替环可依次访问多个不同子环；闭合后将这些子环合并。
3. 可以用一个交替环连接所有子环，也可以先合并其中一部分，再递归寻找其他交替环。所有新增边连接当前不同的子环；新增边不能再删除，删除的原边不能重加，故各次交换的边集合不重叠。不同交替环允许共享节点。
4. 仅当最终得到包含全部工单与虚拟 depot 的单一环，才评估两个方向的业务分数。中间图不写入 State，不按几何正增益裁剪，避免排除业务约束更优的路线。
5. 找不到改善、预算耗尽、取消或成功返回时，都撤销临时边修改；只有保存的完整改善方案才提交。每层复用独立的分量缓冲，不在搜索递归中分配图副本。

若初始连续闭合删除 k 条边并形成 m 个子环，至少还需切 m 条原边；预算不足立即停止该修补。多个交替环可能需要更多切边，总数始终不超过 `k_opt_depth`。因此深度 2～3 不启用修补，4 可以覆盖双桥类非连续交换，6～7 可探索更多三子环修补。预算限制意味着不会穷举所有 LKH 换边类型，也不保证找到任意可行修补。

结果新增 `patching_attempts`（尝试修补的连续闭合次数，不包含内部递归）和 `patched_improvements`（修补后严格改善工作方案的次数，是 `k_opt_improvements` 的子集）。这些计数不等于全局最佳方案改善次数。

回归测试包含：2-opt 局部最优由 3-opt 将成本 60 降至 33；独立枚举证明所有更优路线均为非连续交换的 12 节点实例，由 4-opt 将 120 降至 84；三个子环通过一个交替环以总共 6 条切边修补，或通过两个交替环以 7 条切边修补；总深度/分支上限、嵌套取消后的图恢复、固定工单、硬时间窗优先于路程，以及深度 2～12 的有向矩阵增量分数与 oracle 一致性。

## 停止与复现

FAST 为 30 秒总时限，STANDARD 为 120 秒总时限，DEEP 为全局最佳分数连续 300 秒没有严格改善时停止。两种时限可单独覆盖，同时启用时先到者生效。等分方案及单个线程自己的改善都不会延长全局无改进时间。

计时包含种子方案评分、候选构造、线程启动和搜索，不含输入编译、最终 oracle 解释与文件输出。协作停止在候选评估之间检查，不是硬实时抢占。调用方可用 `AtomicBool` 取消；返回值始终是已记录的最佳方案。线程创建失败或搜索线程 panic 会停止本次任务并返回错误。

seed 固定各线程的随机序列；墙钟预算与操作系统调度会影响循环次数和交流顺序，因此不保证同参数逐次产生完全相同的最终方案。

## 多任务执行

`vrp_model::Executor` 是共享预算的同步执行入口，CLI 也通过该入口求解。库集成时创建一个实例，用 `Arc` 或借用方式让同一进程的任务共享：

```rust
let budget = vrp_model::ThreadBudget::from_cpu_count(available_cpus, 1.8)?;
let executor = vrp_model::Executor::new(budget, 128)?;
let result = executor.solve(&request, &cancelled)?;
```

CPU 数由宿主提供，须反映部署配额与亲和性限制；倍数默认采用 1.0，示例中的 1.8 是显式配置。`128` 是示例排队上限，可按部署内存预算调整；0 表示不能立即执行便拒绝。线程额度覆盖编译、求解、结果解释阶段，排队不占用求解时限。CLI 仅执行一个任务，队列容量为 0。

队列按 FIFO 授予完整额度，避免大线程任务长期饥饿；代价是队首等待期间可能暂时留有空闲额度。排队取消最多每 25 ms 检查一次，移除等待项后唤醒后续请求。错误或异常展开通过 RAII 归还额度。`status()` 暴露已占用线程额度、运行任务数和排队任务数，供宿主监控。

排队前仅解析小型求解参数，矩阵编译延迟到取得额度后；已解码的请求仍由调用方持有。`vrp-worker::Supervisor` 通过 `Executor::reserve` 复用相同额度，完成子进程及 I/O 线程回收后才释放。进程隔离、内存限制与本地进度通道见 [worker.md](worker.md)。

## 与 LKH-3 的关系

LKH-3 通过问题变换和罚函数处理多种受约束 TSP/VRP。这里保留原生多路线表示和已有业务评分，独立实现候选引导、片段移动与扰动，没有引入 LKH 源码或外部可执行文件。对照资料为 Helsgaun 的 LKH-3 技术报告，发布于 **2017-12**，见 [S13](#s13)。

默认 NEAREST 使用普通时长近邻，ALPHA 两种策略使用上述 1-tree 敏感度计算。LKH 的 alpha 候选利用最小 1-tree 与节点罚值评估边的潜力，见 **2000-10** 的 [S08](#s08)。通过 POPMUSIC 构造候选集属于另一条尚未实现的路线，其 **2018-07** 技术报告与适配讨论见 [搜索扩展候选](search-roadmap.md#r09)。

可变深度搜索包含连续交换与通过子环修补构造的非连续交换，保留业务分数作为提交依据。这里按完整路线检查连通性和重算受影响路线，搜索受固定候选/分支预算限制；未实现 LKH 的高效分段树、完整搜索规则及全部参数，当前实现不声称与 LKH-3 等价。对应的 k-opt 子移动论文发布于 **2009-07**，见 [S12](#s12)。

## 合成对照记录

初版记录（提交 `24c43a6`）：2026-10-29，Windows，AMD Ryzen AI 7 PRO 350（8 核 / 16 逻辑线程），Rust 1.99.1，release 构建。生成器固定 seed=741，二维整数点、曼哈顿距离、一个公共起终点；每组搜索 seed=0/1/2，交替执行策略顺序。每次总时限 **1 秒、2 个搜索线程**，包含候选构造；不包含问题生成与 oracle 复核。使用默认业务权重，每个最终方案都经过 oracle 校验。

| 工单 / 车辆 | 基础搜索平均行驶秒数 | 候选搜索平均行驶秒数 | 平均下降 |
|---|---:|---:|---:|
| 100 / 20 | 37,939.3 | 37,536.3 | 1.06% |
| 1000 / 200 | 415,961.7 | 356,749.0 | 14.24% |

两组、两种策略均未出现硬约束罚分或未派单，medium 分分别为 -500 / -5000，因而可以在同级分数相同的前提下比较行驶成本。本场景没有收紧时间窗、技能、容量等条件，3 个种子不足以代表生产分布；这是搜索策略的初步质量对照，不是对 OptaPlanner 或 LKH-3 的性能结论。`corpus/` 当前尚无真实业务语料。

加入 alpha/k-opt 后，同一天、同机器、1000 单 / 200 车、每次 1 秒 / 2 线程 / 3 个种子的再次对照如下。它是独立的一次采样，不能把其数值与初版记录直接当作回归比率：

| 策略 | seed=0 行驶秒数 | seed=1 | seed=2 | 平均 |
|---|---:|---:|---:|---:|
| Baseline | 414,041 | 421,503 | 423,386 | 419,643.3 |
| Nearest | 359,912 | 361,830 | 371,851 | 364,531.0 |
| Alpha | 360,743 | 359,730 | 358,993 | 359,822.0 |
| Alpha + k-opt | 368,987 | 361,414 | 358,039 | 362,813.3 |

本次所有结果仍为 hard=0、medium=-5000、未派=0。Alpha 均完成 1 个矩阵的升级，预处理约 76～80ms；Alpha+k-opt 各次接受 3089 / 3993 / 3963 次工作方案改善。新策略没有在每个种子上都胜过近邻，短路线上的换边开销也占用总预算，因此保留 NEAREST 为默认。后续应使用真实时间窗、车型、容量与不同路线长度的语料，按 30 / 120 秒档验证收益。

复现命令：

```bash
cargo run --release -p vrp-core --example search_bench -- 100 20 1000 2 3
cargo run --release -p vrp-core --example search_bench -- 1000 200 1000 2 3
```

例程现在轮换四种策略，输出每个种子的分数、未派数量、评估次数、预处理成本、k-opt 改善次数、修补尝试和修补改善次数。上表记录于加入非连续换边之前，不能当作本次实现的性能结论。

2026-10-30 加入修补后，在同机器再次运行 1000 单 / 200 车、每次 1 秒 / 2 线程、默认深度 5，结果如下（独立采样，不作跨日期回归比率）：

| 策略 | seed=0 行驶秒数 | seed=1 | seed=2 |
|---|---:|---:|---:|
| Baseline | 418,096 | 421,739 | 413,799 |
| Nearest | 365,920 | 371,053 | 362,548 |
| Alpha | 363,571 | 369,263 | 364,257 |
| Alpha + k-opt + patching | 372,977 | 368,098 | 368,452 |

所有结果 hard=0、medium=-5000、未派=0，均通过 oracle 校验。Alpha+k-opt 分别尝试修补 9292 / 11632 / 12059 次，接受修补改善 860 / 1040 / 1088 次。这证明修补路径参与实际搜索，但短预算下最终解没有稳定胜过近邻；工作方案改善次数也不能视为全局收益。因此保留 NEAREST 默认，真实业务收益仍需语料验证。本次 Windows 工作区 82 项测试、核心 Release 单元测试和 workspace/all-targets Clippy 通过。

测试用合成请求由代码生成，CLI 集成测试临时文件放在项目 `corpus/` 下并自动清理。真实业务语料和其结果继续按仓库规则留在 `corpus/`，不进入版本控制。

## 参考文献与日期依据

以下日期核对于 **2026-11-02**。旧期刊的双月卷期与出版社页面日期同时列出；日期不用于声称算法的最早提出时间。

1. <a id="s01"></a> **S01 — Marius M. Solomon.** [Algorithms for the Vehicle Routing and Scheduling Problems with Time Window Constraints](https://doi.org/10.1287/opre.35.2.254). *Operations Research*, 35(2), 254–265。**1987-04**，出版社列出 1987-04-01，卷期为 1987 年 3–4 月。
2. <a id="s02"></a> **S02 — Martin W. P. Savelsbergh.** [The Vehicle Routing Problem with Time Windows: Minimizing Route Duration](https://doi.org/10.1287/ijoc.4.2.146). *ORSA Journal on Computing*, 4(2), 146–154。**1992-05**，出版社列出 1992-05-01，卷期为 Spring 1992；[早期技术报告](https://pure.tue.nl/ws/portalfiles/portal/1661516/345296.pdf)封面为 **1991-01**。这里用于说明边交换邻域，不将其认定为 relocate / swap 的首次提出。
3. <a id="s03"></a> **S03 — G. A. Croes.** [A Method for Solving Traveling-Salesman Problems](https://doi.org/10.1287/opre.6.6.791). *Operations Research*, 6(6), 791–812。**1958-12**，出版社列出 1958-12-01，卷期为 1958 年 11–12 月；作为 2-opt 的经典来源引用。
4. <a id="s04"></a> **S04 — Ilan Or.** *Traveling Salesman-Type Combinatorial Problems and Their Relation to the Logistics of Regional Blood Banking*. Northwestern University，博士学位论文，**1976-06**。题名和月份据 [Pierskalla 作者资料中的参考文献 [33]](https://www-apache.anderson.ucla.edu/faculty_pages/william.pierskalla/Chronological_Bank/Health_Chro/25_Hlt_Chro.pdf)核对；[Cohen、Pierskalla 的后续论文参考文献 [9]](https://doi.org/10.1046/j.1537-2995.1979.19580059802.x)也记载 June 1976。本次未取得学位论文全文，不据此声称逐项复现原文。
5. <a id="s05"></a> **S05 — Olivier Martin, Steve W. Otto, Edward W. Felten.** [Large-Step Markov Chains for the Traveling Salesman Problem](https://digitalcollections.ohsu.edu/record/3699/files/csetech-16.pdf). Oregon Graduate Institute，Technical Report CS/E 91-016，封面 **1991-06**；同题期刊版发表于 *Complex Systems*, 5(3), 299–326。这里采用可核对封面月份的技术报告版本，作为双桥扰动的来源。
6. <a id="s06"></a> **S06 — Gerhard Schrimpf, Johannes Schneider, Hermann Stamm-Wilbrandt, Gunter Dueck.** [Record Breaking Optimization Results Using the Ruin and Recreate Principle](https://doi.org/10.1006/jcph.1999.6413). *Journal of Computational Physics*, 159(2), 139–171。**2000-04**，正式卷期日期为 2000-04-10。
7. <a id="s07"></a> **S07 — Edmund K. Burke, Yuri Bykov.** [The Late Acceptance Hill-Climbing Heuristic](https://doi.org/10.1016/j.ejor.2016.07.012). *European Journal of Operational Research*, 258(1), 70–78。**2017-04**，正式卷期日期为 2017-04-01；采用期刊定稿版本，不能把此日期视为 LAHC 最早公开时间。
8. <a id="s08"></a> **S08 — Keld Helsgaun.** [An Effective Implementation of the Lin–Kernighan Traveling Salesman Heuristic](https://doi.org/10.1016/S0377-2217(99)00284-2). *European Journal of Operational Research*, 126(1), 106–130。**2000-10**，正式卷期日期为 2000-10-01；alpha-nearness 和候选集引导的主要参考。
9. <a id="s09"></a> **S09 — Michael Held, Richard M. Karp.** [The Traveling-Salesman Problem and Minimum Spanning Trees](https://doi.org/10.1287/opre.18.6.1138). *Operations Research*, 18(6), 1138–1162。**1970-12**，出版社列出 1970-12-01，卷期为 1970 年 11–12 月；1-tree 与节点罚值的基础来源。
10. <a id="s10"></a> **S10 — Michael Held, Richard M. Karp.** [The Traveling-Salesman Problem and Minimum Spanning Trees: Part II](https://doi.org/10.1007/BF01584070). *Mathematical Programming*, 1, 6–25。**1971-12**，Springer 列为 December 1971；迭代优化 1-tree 下界的参考。
11. <a id="s11"></a> **S11 — S. Lin, B. W. Kernighan.** [An Effective Heuristic Algorithm for the Traveling-Salesman Problem](https://doi.org/10.1287/opre.21.2.498). *Operations Research*, 21(2), 498–516。**1973-04**，出版社列出 1973-04-01，卷期为 1973 年 3–4 月；可变深度换边的经典来源。
12. <a id="s12"></a> **S12 — Keld Helsgaun.** [General k-opt Submoves for the Lin–Kernighan TSP Heuristic](https://doi.org/10.1007/s12532-009-0004-6). *Mathematical Programming Computation*, 1, 119–163。**2009-07**，首次在线发表 2009-07-01；[作者论文 PDF](https://www2.seas.gwu.edu/~simhaweb/champalg/tsp/papers/Helsgaun2.pdf)首页列出该日期，§5.2 描述 cycle patching。
13. <a id="s13"></a> **S13 — Keld Helsgaun.** [An Extension of the Lin-Kernighan-Helsgaun TSP Solver for Constrained Traveling Salesman and Vehicle Routing Problems](https://forskning.ruc.dk/en/publications/an-extension-of-the-lin-kernighan-helsgaun-tsp-solver-for-constra/). Roskilde University，技术报告。**2017-12**，大学资料库列为 2017-12-29；仅用于与完整 LKH-3 比较，不表示本项目实现了整套算法。
