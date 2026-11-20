# 历史案例整理与回放

[完整求解评测](evaluation.md)使用 `benchmarks/audit_corpus.py` 复核来源和候选配对，`prepare_history.py` 只从已审核的原请求构造私有迁移轮次。结果文件仅可补矩阵，时间窗、时长及班次保持原请求。静态历史回放与同预算计时分别记录；私有输入、逐例结果及诊断全部留在 `corpus/evaluation/`。

工作台历史迁移使用 `scripts/import_scenarios.py --source <scenarios目录>`，默认输出至私有 `corpus/workbench`，可选 `--server` 上传原生场景。自动估算补齐、来源记录、坐标覆盖及幂等导入规则见 [开放地图与历史场景](maps-and-import.md)。该流程独立于下面的既有基准索引，不覆盖原来的人工配对和报告。

私有业务语料、索引和报告统一放在 `corpus/`，受 `.gitignore` 排除；只提交工具、通用测试及不含客户信息的说明。旧项目的 Java 测试不会直接变成 Rust 测试，迁移的是输入数据和有证据的回归意图。

## 目录与来源

`scripts/organize_corpus.py` 读取指定本地 vrp-0 仓库的 `scenarios/` 和 `src/test/resources/` 中 JSON、CSV、XLSX 文件。原件按 SHA-256 去重复制，不改字节，所有来源路径及 Git HEAD 记录在私有索引中；副本哈希才是具体内容依据，HEAD 不表示未提交文件已纳入该提交。

```text
corpus/
├─ .gitkeep
├─ README.md                 私有案例目录与校验摘要
├─ index.json                来源、哈希、分类、排除原因与候选配对
├─ bench.txt                 请求相对路径，一行一条
├─ raw/
│  ├─ request/               明确的原始请求
│  ├─ result/                历史结果，不直接作问题输入
│  ├─ snapshot/              来源/角色不明确的计划快照
│  ├─ support/               POI、表格等辅助资料
│  └─ invalid/               空文件和损坏 JSON，原件保留
└─ reports/                  导入、评分、回放、基准输出
```

索引仅为本地开发资料格式，不是服务或引擎契约。重跑整理使用新的输出目录，避免覆盖已有人工标注及报告。工具不会读取生产服务的数据库、凭据或其他业务目录。

## 整理与校验

```powershell
cargo build --release -p vrp-cli
python scripts/organize_corpus.py --source <旧仓库目录> --output corpus --vrp target/release/vrp.exe
# 可重复 --source 纳入另一个本地镜像；没有 --vrp 时只分类，不宣称校验通过。
cargo run --release -p vrp-cli -- bench
```

校验调用 `vrp eval`，覆盖导入、请求编译和全量评分；**不等于执行过 30/120 秒求解或证明方案质量提升**。损坏、角色不明确、超出 1000 单/200 车目标及导入失败的记录不进入默认清单，原因保留在索引。历史结果候选先按同目录、明确文件名关系、工单和非虚拟车辆 ID 集合筛选，仍不自动认定为同输入的标准答案。

`bench` 在目录存在 `bench.txt` 时只读取选中请求；没有清单时保留递归读取 JSON 的旧开发用法。清单路径必须指向语料目录内的 JSON 文件，重复路径只运行一次。清单为空、所选文件不是场景、导入/编译失败或增量评分不一致时返回失败，不能靠跳过全部文件得到一次“通过”。

## 请求与结果的正确配对

```powershell
target/release/vrp.exe eval <原始请求.json> --solution <历史结果.json> --matrix <历史结果.json>
```

请求提供约束、班次和时间窗，结果只提供指派及显式指定的矩阵。历史 v1 结果可能改写班次起点、时间窗、时长和矩阵；v1/v2 评分语义也有差异。因此回放成功表示该组合能按 v2 校验评分，不表示重现 v1 分数、历史输入完全一致或方案满足所有业务规则。未配对结果和中途修改过约束的后续结果应单独保留，不能只按相同工单 ID 自动当作标准答案。

## 动态回放的边界

既有静态请求、结果和“改参数再求解”测试不等于执行过程日志。实时重排验收需要出发、到达、服务、完成、取消、装卸、故障、确认等事件及发生顺序；要求见 [realtime-planning.md](realtime-planning.md)。合成动态回归与真实业务事件回放分别标记，不能以静态案例数量宣称动态能力已通过。
