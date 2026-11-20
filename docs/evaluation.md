# 可复现的多场景求解评测

本套件比较指定源码和配置的 vrp-0 / OptaPlanner 与 vrp-1 的完整求解行为。`vrp bench` 的评分吞吐、`search_bench` 的内部策略实验不属于跨版本同预算证据。正式结果见 [第一轮报告](benchmarks/round-1/report.md)，机器可读分布见 [summary.json](benchmarks/round-1/summary.json)。英文 README 继续作为默认入口。

## 审计与事实源

首轮使用 vrp-1 基础提交 `eb6333f90e898d47f24424d304a5ced7dec5a12d`，旧版源码锁定为 `8cc39f6be1a1134c2749589b6e9a5abb2c5b50f0`。评测从旧版 `src/main/java` 重新编译应用代码，依赖取自固定的 Quarkus 构建。依赖 JAR 与编译后 class 的 SHA-256 见[评测产物指纹](benchmarks/round-1/artifact-hashes.json)。

本轮增加监测代码，故 vrp-1 版本由基础 commit、源码归档哈希、二进制哈希共同确定，不能仅用基础 commit 声称复现新增套件。每轮在本地保存 `environment.json` 和 `source.zip`，记录源码快照、配置、版本、构建条件及指纹。公开报告保留首轮构建指纹。使用当前仓库和下方命令可按同一方法运行新一轮评测。生产评分、proto 契约、默认搜索策略没有改变。core 新增可选的内存事件队列，没有外部依赖、I/O、async 或全局状态。

`audit_corpus.py` 重新核对私有副本哈希和来源，按既有人工候选配对比较时间窗、时长、班次、矩阵、权重，逐对执行历史静态回放。相同 ID 不能证明相同问题；有改写的结果保留为历史参考。没有将历史结果或其旧分数当成本轮同预算运行。首轮算法场景使用公开固定种子合成数据，历史数据单独审计、回放；真实业务分布和动态现场事件仍需另设私有评测轮次。

## A：共同语义

共同目标必须在搜索时就相同，而非仅最终重评分。权重全部显式设置：容量、单数上限、技能、资质、网点为 1 hard；未派为 1000 medium；行驶秒数为 1 soft。紧窗例另启用晚开始分钟数 1 hard。其他目标关闭，包括固定成本、均衡、改派、日期、依赖、班次结束、不可达、新版执行约束。资质从默认 medium 改为此实验的 hard，对两边一致，生产默认不变。

只允许一维整数配送需求，容量罚分单位 1、无容差；不启用旧版有差异的多维峰值和取送混合容量。所有路径秒数、服务时长、开始时间和时间窗是 60 的整数倍，因此任意访问顺序的截断/向上取整分钟量相等。允许早到等待，关闭旧版忽略等待的“结束服务”约束。全部车辆技能非空，避开旧版空技能行为差异。只用完整、有限、单 profile、同址往返矩阵，不使用任何 fallback 或地图服务。距离仅作为业务指标，不参与此目标。

小案例门禁执行 80 次固定种子采样核对，覆盖 64 种不同指派和访问顺序，包含虚拟未派、容量/单量/技能/资质/网点/时间窗违约，同时核对旧版真实评分器、v2 oracle 和 Python 独立评分。冻结后再核对 12 份完整共同初始方案。共同 profile 的输入检查会拒绝未验证约束、秒级时间、未冻结矩阵及其他语义。Java 使用真实旧版领域对象、ConstraintProvider、影子变量监听器、构造式建解及 `SolverWrapper.LocalSearchConfig_1`（list change + list swap，tabu 4、SA 2hard/2medium/100soft、acceptedCountLimit 4），只显式覆盖种子、预算和线程数。vrp-1 使用默认 nearest 搜索。虚拟车辆只在适配器中映射未派，不进入 core。

冷建解：vrp-1 无 initial_solution；旧版所有列表初始为空，由原构造阶段指派。继续优化：两边使用同一份公开 round-robin 初始方案，包括完全相同的访问顺序；初解不由任一被测求解器生产。两种模式分别计数、绘图、比较。旧版构造尚未完成的 `initScore < 0` 单独记 `no_solution`，不得丢掉初始化层后把三层分数当成有效业务分数。

## B：迁移和能力验收

多维取送、前置依赖和执行变化使用 v2 原生语义，旧版逐次标记 `unsupported`，不参与 A 排名。执行案例是明确标记的**合成冻结执行投影**：实际载荷、仅允许货物所在车辆、不可用车辆、完成前缀、已完成依赖边界、锁定后缀前缀、原顺序/预约基准均在请求里显式保存。取消已装货配送单变为回仓卸货辅助访问，不释放实际载荷。车辆故障不假造货物转移。workspace 的服务事件测试另覆盖插单、取消、故障、交接、事实锁、幂等、采用及重启，不能将静态投影称为真实事件回放。

可用标准预先定义为：结构和统一校验通过、hard=0、全部要求的业务单已派。Hard=0 不自动表示全派。故障例单独要求被故障车货物锁定的工单明确未派，其他业务单全部完成安排；已知不可行例要求无虚假可执行违反、至少 46 单未派（两车各限 2 单）。这些是能力验收，不能计入全派业务成本改善。未达标准保留为 `business_standard_not_met`。

## 场景与第一轮运行量

配置唯一入口是 [`benchmarks/scenarios.json`](../benchmarks/scenarios.json)。开发清单和统计 JSON 不是新的业务契约，业务输入/输出继续用 `.proto` 生成的 ProtoJSON。

| 集合 | 场景 | 单 / 车 | 变化 |
|---|---|---:|---|
| A | a01 基础 | 50 / 10 | 短路线 |
| A | a02 城市密集 | 200 / 8 | 长路线、较紧单量资源 |
| A | a03 跨区多网点 | 480 / 80 | 四网点、短路线 |
| A | a04 规模增长 | 1000 / 100 | 跨区、异构单量上限 |
| A | a05 技能资质 | 200 / 20 | 容量和匹配叠加 |
| A | a06 紧窗等待 | 50 / 4 | 容量、等待、时间窗叠加 |
| B | b01 取送多维 | 200 / 16 | 独立维度峰值、班次 |
| B | b02 前置 | 50 / 6 | 跨路线依赖、窗口、班次 |
| B | b03 插单 | 200 / 20 | 执行载荷与预约变化 |
| B | b04 取消 | 50 / 6 | 已装货保留、辅助卸货 |
| B | b05 故障完成锁 | 50 / 6 | 故障货物、完成依赖、承诺前缀 |
| B | b06 不可行边界 | 50 / 2 | 资源不足、技能冲突、不可达 |

生成 seed=7412026；搜索 seed=11/29/47/71/101。A：6 例 × 2 启动方式 × 5 种子 × 2 引擎 = 120 次。B：6 例 × 5 种子 = 30 次 vrp-1 验收，另保存 30 个旧版不支持记录。合计 **150 次实际计时、180 个预期记录**，每次 30 秒、单线程，共 75 分钟名义搜索预算。12 个 Java 场景/启动方式分组各预热 3 × 20 秒，额外约 12 分钟；加编译与 I/O，预计约 90 分钟。先通过小规模端到端工具验证，不将短预算验证计入正式结果。

## 时间、资源和事件口径

求解预算包含候选邻域预处理、初始评分、构造、搜索；Java 的初始影子传播从求解预算扣除，事件时刻加回这段实际耗时，模型编译独立计时。输入读取、模型编译/solver 创建、结果解释、输出、进程启动分别留阶段记录。地图下载、地理编码、路网准备、网络和排队不进入计时；运行时没有此类调用。生成矩阵是冻结的数学合成路程，不能当成实际城市交通时长。

主对照串行，同一轮没有两个被测求解器同时运行。分组轮换两引擎先后顺序。Windows Job Object 限制 2 GiB 进程树提交内存、同一逻辑 CPU 亲和性，关闭句柄回收整个进程树。Java 设置 ActiveProcessorCount=1、SerialGC、Xms128m/Xmx1536m；JIT/GC 与求解共同受单 CPU 限制，未把额外 JVM 线程算成免费资源。记录 JDK、JIT 预热分轮编译毫秒、冷启动/第一轮模型编译，预热不混入测量搜索时间。预热长度是明确的实验配置，日志保留是否仍有编译活动。

CPU 使用 Windows Job 的 user+kernel 累计，包含子进程；Java 每条指令另用 OperatingSystemMXBean 精确记录 user+kernel 差值。Rust 的单次进程 CPU 包含启动、输入、编译、搜索与输出，Java 预热后指令 CPU 不含 JVM 启动/预热，报告注明这点。高水位提交内存由内核采集；Java 为同组 JVM 生命周期（含预热及前序种子）峰值，Rust 为单次进程峰值，**两者作用域不同**。另按约 20ms 采样每条指令的 RSS 峰值，可能漏掉短峰，不把 RSS 和 committed 混为一个数字。本轮硬资源限制与采集只在 Windows 验证；Linux/Docker 后端显式返回不支持，避免忽略内存上限后仍发布比较。

三轮固定预热已单独执行和归档，但末轮仍有 JIT 编译活动；本轮只能称为该固定预热配置下的比较，尚未证明完全编译后的稳态收敛。更长预热的敏感性复核仍待执行，不能从这些结果推算完全稳态的提速倍率。

真实 incumbent 事件含搜索内实际时刻、三层分数、已派数量。Rust 内存队列仅在显式请求时启用，宿主批量导出；Java 使用 best-solution listener。不会从最终分数伪造曲线，也不把轮询时刻当成实际改善时刻。首次可用方案由已初始化且满足预定标准的真实事件确定；未达标保留 null 和右删失标记。最终统一校验结构、重复/漏单、约束、未派、行驶时间/距离、预约以及适用的改派/改序/预约变化；每个最终方案另交 v2 独立 oracle 重算。失败、超时、CPU 额度终止、取消、无完整解、校验失败、业务标准未达和不支持分别落盘。

## 统计与结论边界

每个实例/启动方式/引擎保留五个观测值、中位数、四分位数、最小最大、失败数和删失。配对 W/T/L 按同种子、同约束和相同全派水平比较，成本比率只用两边均全派、hard=0、统一校验通过的配对，不跨实例累加原始分数。失败不从成功率或计划记录数中消失。真实质量—时间图分开展示未派和全派后成本；五个种子未全部可行前不画该组中位成本曲线。

达到同等质量：除预先定义的首次全派可行标准，报告另给出事后配对目标——两边最终全派可行成本中较差的那个，并在各自真实事件中找首次达到时刻；这不是预先指定的行业质量阈值。未达到时右删失，不外推所需时间。五个种子只提供首轮分布，不声称统计总体胜率、生产 SLA、全局最优或对 OptaPlanner 框架整体的优劣。

## 复现命令

从仓库根目录执行，Python 3.14、JDK 21、Rust 1.99.1。旧仓库和已构建依赖目录为参数，不假定相对位置。依赖 JAR 原件需要与 manifest 指纹一致；SNAPSHOT 名称相同还不足以复现。

```powershell
python -m pip install -r benchmarks/requirements.txt
cargo build --release -p vrp-cli --bins
python benchmarks/build_legacy.py --source <vrp-0-checkout> --dependencies <quarkus-app-dir> --output corpus/evaluation/build/java
python benchmarks/audit_corpus.py --output corpus/evaluation/history-audit --vrp target/release/vrp.exe
python benchmarks/gate.py --adapter target/release/vrp-bench-adapter.exe --legacy-build corpus/evaluation/build/java/build.json --output corpus/evaluation/gate
python -m unittest discover -s benchmarks -p test_suite.py -v
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python benchmarks/run.py prepare --output corpus/evaluation/round-1-final --legacy-build corpus/evaluation/build/java/build.json
python benchmarks/run.py run corpus/evaluation/round-1-final
python benchmarks/run.py status corpus/evaluation/round-1-final
python benchmarks/report.py corpus/evaluation/round-1-final --public-output docs/benchmarks/round-1
```

没有旧版时省略 `--legacy-build`，照常完成 vrp-1，旧版显式不支持，报告不产生跨版胜负。`prepare` 只接受新的输出目录，冻结问题、矩阵、转换、初解、配置、版本和计划记录，并为本轮复制执行文件、全部 Java class 与依赖 JAR。`run` 验证指纹、按唯一运行 ID 保存 `attempt-001` 等目录、原子发布每条 record，并持续更新 progress。重复执行跳过所有已完成状态；`--retry-failed` 建新 attempt，保留原失败。未有终态的旧 attempt 在恢复时记 interrupted。创建轮次目录下 `CANCEL` 请求取消并回收进程树；清除该文件后执行同一 run 命令继续；补跑已标记取消或失败的记录时加 `--retry-failed`。

扩展时复制配置到 `corpus/`，显式选择 1000/10000/120000ms、线程数及 `cpu_budget_seconds`，准备一个新轮次。CPU 额度是实测进程 CPU 的终止边界，线程数与亲和性数量相同；仍保存实际 CPU/墙钟和检查粒度导致的超额。不会默认展开所有预算/线程的笛卡尔积。

原始请求、历史数据、逐例结果、命令中的本地路径、含私有参数的配置快照和私有报告只保存在被忽略的 `corpus/`。`report.py` 公共导出仅允许本合成套件；私有轮次不允许直接发布。公开仓库纳入工具、生成规则、合成结果汇总和曲线。

## 私有历史迁移计时

历史原请求与配对结果经指纹和角色审计后，可用 `prepare_history.py` 构造单独的私有轮次。原请求提供业务事实和原有初始安排；配对结果仅补充矩阵，不覆盖班次、时长或窗口。坐标不一致时拒绝矩阵覆盖，转用原请求和冻结的本地估算。转换后展开 v2 默认权重，保留警告、补边数量、双来源指纹和索引依据。此轮从原请求的初始安排继续搜索，标记 `request-initial`；它不是旧版同预算对照。逐例报告和事件曲线强制留在 `corpus/`。

```powershell
python benchmarks/prepare_history.py --corpus corpus --case-id <已审核案例ID> --adapter target/release/vrp-bench-adapter.exe --output corpus/evaluation/history-input
python benchmarks/run.py prepare --config corpus/evaluation/history-input/config.json --output corpus/evaluation/history-round-1 --legacy-build corpus/evaluation/build/java/build.json
python benchmarks/run.py run corpus/evaluation/history-round-1
python benchmarks/report.py corpus/evaluation/history-round-1 --public-output corpus/evaluation/history-round-1/report
```

故障例的首次业务标准允许预先声明的故障货物未派，另存 `first_business_standard_ms`，不伪装成首次全派。已知不可行例的全派标准明确不适用。启动预算核查中止的目录、短端到端验证与正式轮次各自独立，原记录保留，不混入正式统计。

同等质量同时保留两个明确标记的事后成本目标：配对中较差和较好的最终成本。`equal_quality_stricter` 给出较严格目标的真实达到时刻、命中数和删失标记；不能因为对方在 30 秒内没有达到，就推算它需要多少倍时间。两种事后成本目标均与预先定义的业务可用标准分开。

## 首轮交付与验证

公开轮次为 12 例、150 次正式计时、30 个不支持记录；另有一例私有历史迁移的 5 次正式计时及 5 个旧版不支持记录。所有计时均按 30 秒、单线程执行，历史逐例结果不进入公开报告。公开报告只说明指定版本、目标和配置的合成比较。

已执行 `cargo test --workspace`（210 项）、workspace/all-targets Clippy，以及 21 项场景生成、映射、校验、CPU/内存限制、回收、取消、恢复、统计和报告隐私测试。Java 重试保留前次批次日志，并确保启动失败也为当前 attempt 写入终态；超时和损坏输出仍保留真实改善事件，损坏输出以校验失败落盘。`verify_campaign.py` 检查计划完整性、输入和二进制指纹、真实改善事件单调性、最终结果、CPU/内存证据及实际求解时长；不会因业务失败而删除记录。

```powershell
python benchmarks/verify_campaign.py corpus/evaluation/round-1-final
python benchmarks/verify_campaign.py corpus/evaluation/history-round-1
python benchmarks/update_readmes.py --summary docs/benchmarks/round-1/summary.json
```

README 图表只从公开的 `summary.json` 和 `traces.json` 重建，中英文各生成最终成本改善、冷建解曲线、继续优化曲线、同等质量耗时、CPU/内存五张 SVG。生成器 `benchmarks/readme_figures.py` 使用真实最佳方案事件的阶梯函数，只有全部配置种子全派且无硬违约后才显示成本；各场景独立纵轴。数据和绘图代码指纹保存在 `readme-figures.json`，不读取私有语料。可给 `update_readmes.py` 加 `--preview-dir corpus/evaluation/visual-qa/readme` 生成本地 PNG，检查排版；该命令同步重建两份 README 的评测节和图表。

本轮尚未执行 1/10/120 秒或同 CPU 额度的多线程扩展；没有将合成执行投影或静态历史请求称为现场动态回放。
