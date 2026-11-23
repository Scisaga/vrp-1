# 独立任务服务

地图启动配置：`--map-config <MapConfig ProtoJSON文件>`；默认内嵌 MapLibre，优先使用离线底图，缺包期间以在线 OSM 底图过渡。底图准备、导入与断网部署见 [离线底图](offline-basemaps.md)。历史场景批量导入、在途矩阵来源与任务道路折线接口见 [开放地图与历史场景](maps-and-import.md)。可直接使用已有矩阵或估算矩阵求解。

`vrp-server` 提供本机多场景、多任务 HTTP 服务及内嵌浏览器工作台。包括场景编辑、求解、执行回报、路线示意、人工/自动采用和历史快照回放。所有应用消息与 HTTP 路径定义在 [app.proto](../proto/vrp/app/v1/app.proto)，求解语义沿用 engine.proto。

车辆出发后的实时重排已接入同一服务，执行语义和边界见 [realtime-planning.md](realtime-planning.md)。启动后打开 `http://127.0.0.1:8080`，通过场景列表顶部的新建或导入图标准备业务数据。

## 启动

从项目根目录运行：

```powershell
cargo run --release -p vrp-server -- --bind 127.0.0.1:8080 --data-dir var --cpu-multiplier 1.8
```

SQLite 随程序编译，界面与服务一同提供。同一个 `vrp-server` 可执行文件以内部 `worker-child` 模式启动隔离求解进程。默认数据目录 `var/` 已加入忽略列表，测试数据仍在 `corpus/`。

Windows 可执行 `powershell -File scripts/package-windows.ps1`，在被忽略的 `target/dist/` 生成 ZIP，包含服务、CLI、协议、说明和 `start.cmd`。解压后运行 `start.cmd`，数据写入同目录 `var/`。Linux 可原生 `cargo build --locked --release -p vrp-server` 后部署二进制；本次未验证 Linux 容器发行物。

现有服务占用默认 release 可执行文件时，可用 `-BuildTargetDirectory target/geo-release-build` 隔离打包。已有 PBF 目录通过 `--geo-local-dir C:/maps` 登记，完成后按哈希持久保存并跨场景复用；发行包不包含语料和地图大文件。

| 参数 | 默认 | 说明 |
|---|---:|---|
| `--bind` | `127.0.0.1:8080` | 监听地址 |
| `--mcp-origin` | 未启用 | 启用同端口 `/mcp`，值为客户端可访问的 HTTP(S) 来源。见 [WorkBuddy 与 Codex 接入](workbuddy-mcp.md)。 |
| `--mcp-threads` | 总额度内最多 4 | 每次 MCP 求解使用的线程数，不超过 `--total-threads`。 |
| `--mcp-search-ms` | 30000 | MCP 求解的搜索预算，单位毫秒。 |
| `--mcp-search-strategy` | baseline | MCP 求解策略，可设为 `baseline` 或 `nearest`。 |
| `--mcp-default-scenario` | 未绑定 | `ID@VERSION`。允许初排工具省略场景和版本，启动时校验该版本存在。 |
| `--data-dir` | `var` | SQLite 数据目录；只允许一个服务实例持有 |
| `--cpu-multiplier` | 1.0 | 总线程额度为 `max(1,floor(CPU×倍数))`，也可通过 `VRP_CPU_MULTIPLIER` 设置 |
| `--total-threads` | 未设置 | 显式指定部署额度，优先于 CPU 倍数，适合容器配额 |
| `--max-running` | 4 | 同时执行的进程数上限，还受线程和内存准入限制 |
| `--max-queued` | 128 | 等待执行的任务数上限，必须大于 0 |
| `--worker-memory-mib` | 1024 | 每个子进程内存限制，计量方式见 worker.md |
| `--memory-budget-mib` | 4096 | 应用内存准入预算，不是 OS 硬限制 |
| `--max-storage-mib` | 4096 | 输入/结果、当前执行快照及历史命令/快照的逻辑字节上限 |
| `--max-records` | 10000 | 场景版本、任务、当前运营及执行历史记录合计上限 |
| `--basemap-auto` | true | 首次打开已保存场景地图时自动准备原生 0–14 级；启动不批量下载 |
| `--basemap-catalog-url` | Protomaps 官方构建目录 | 兼容 v4 的目录 JSON，也可直接指定支持 Range 的 PMTiles URL |
| `--basemap-archive-base-url` | `https://build.protomaps.com` | 目录中构建文件的根地址 |
| `--basemap-download-mib` | 500 | 单次新增传输预算；元数据和索引也计入 |
| `--basemap-storage-mib` | 10240 | 底图独立磁盘总额度，含临时峰值 |
| `--geo-prefetch` | true | 保存场景后按地点范围后台准备路网包 |
| `--geo-catalog-url` | Geofabrik 全球官方目录 | 可指定本地镜像；测试使用本地合成目录 |
| `--geo-local-dir` | 未设置 | 扫描并登记已有 PBF，供多个场景复用 |
| `--geo-download-mib` | 500 | 单场景自动新增下载预算 |
| `--geo-memory-mib` | 1536 | 每个路网计算子进程内存额度 |
| `--geo-road-wait-ms` | 180000 | 场景首次等待路网的上限，超时后显式补齐估算矩阵 |

每个运行任务预留“子进程内存额度 + 4 倍 IPC 帧上限”，宿主另留 512 MiB。默认帧上限 64 MiB，因此默认内存配置最多准入 2 个并行任务，即使 `max-running=4`。这是保守的准入估算；Rust 对象、JSON 展开和 SQLite 缓存不由它精确计量，部署方仍应设置容器/主机内存总限额。多个独立服务实例须事先切分机器额度。

API 请求体与归档 SolveRequest 上限为 32 MiB，结果受 64 MiB IPC 帧上限约束。普通 HTTP 请求/下载最多并发 4 个，后台解码/数据库/编码操作另限 4 个，SSE 最多 128 个；额度不足返回 429。慢客户端下载会占住传输额度，断连不会提前释放仍在执行的后台操作额度。读取请求体最多等待 10 秒。

默认仅允许本机访问。监听非回环地址必须提供至少 16 个可见 ASCII 字符的 `VRP_API_TOKEN`。本机直接访问 `127.0.0.1` 等回环地址免令牌，即使服务已配置令牌；非回环地址访问的业务 API 要求 `Authorization: Bearer <token>`。免令牌仅适用于真实连接对端与 Host 均为回环地址（含 localhost），且没有 Forwarded、X-Forwarded-For 或 X-Forwarded-Host 转发头的请求；无法确认连接来源时继续鉴权，不能只伪造 Host 绕过。工作台“访问设置”接收令牌，仅保存在当前页面内存中；页面静态外壳公开。公网入口由反向代理提供 HTTPS，转发时保留原 Host 并传递转发头。未设置令牌时 Host 必须为 localhost/回环 IP；Origin 必须同源。工作台有界轮询，SSE 仍供支持 Authorization 的 fetch 流/API 客户端使用。

## 最小求解示例

以下合成请求可直接在另一个 PowerShell 窗口执行。坐标和矩阵均为示例，不包含客户数据：

```powershell
$request = @{
  request = @{
    problem = @{
      locations = @(@{ id = 'depot'; lng = 0; lat = 0 })
      matrices = @(@{ durations = 'AAAAAA=='; distances = 'AAAAAA==' })
      agents = @(@{ id = 'vehicle-1' })
      tickets = @(@{
        id = 'ticket-1'; originalAgent = 0; pinned = $true
        latestStart = '86400'; latestEnd = '86400'
      })
    }
    options = @{ preset = 'SOLVE_PRESET_FAST'; threads = 2; timeLimitMs = '1000' }
  }
  idempotencyKey = 'example-run-1'
} | ConvertTo-Json -Depth 12
$job = Invoke-RestMethod http://127.0.0.1:8080/v1/jobs -Method Post -ContentType application/json -Body $request
Invoke-RestMethod "http://127.0.0.1:8080/v1/jobs/$($job.id)"
# 有最终结果之后：
Invoke-RestMethod "http://127.0.0.1:8080/v1/jobs/$($job.id)/result" -Headers @{ Accept = 'application/json' }
# 运行中可取消：
# Invoke-RestMethod "http://127.0.0.1:8080/v1/jobs/$($job.id)/cancel" -Method Post
```

HTTP POST 接受 `application/json`（ProtoJSON）或 `application/protobuf`；默认响应 JSON，指定 `Accept: application/protobuf` 可取二进制。`/request` 和 `/result` 默认返回 protobuf，指定 JSON Accept 则转换。JSON 中 int64/uint64 输出为字符串、bytes 是 base64、枚举使用协议名称；缺省标量可能被省略，客户端应通过生成的类型补默认值。

请求未指定档位时仍为 FAST 30 秒总时限；STANDARD 为 120 秒总时限，DEEP 为连续 300 秒无全局最佳改善，总时限默认不限。服务不改变核心停止条件，排队、请求解码和编译不消耗搜索时限。

## 场景与任务

一级导航为“场景｜路网包管理｜任务中心｜服务设置”。任务中心使用 `/v1/tasks` 查询求解和地理任务，保留历史求解 ID 和冻结输入，求解状态仍由原有任务记录唯一管理。通用任务包含类型、进度、父子与依赖、错误和可下载产物；求解专属信息在 `solver` 字段中。

- `POST /v1/scenarios/{id}/geo-plan` 接收 `GeoPlanRequest`：`expectedVersion`、`idempotencyKey` 和可选求解参数。后台依次准备路网、固化矩阵、CAS 保存场景版本、求解及生成道路折线；保存请求不等待地图下载。
- `GET /v1/tasks` 支持 `page_size`、`before_id`、`scenario_id`、`kind`、`state`；`/{id}`、`/{id}/events`、`/{id}/cancel`、`/{id}/retry` 统一查询、进度、取消和重试。`/{id}/artifacts` 列出产物，`/{id}/artifacts/{artifact}` 下载。
- `POST /v1/tasks` 接收 `SubmitTaskRequest {input: TaskInput, idempotencyKey}`，按类型提交场景规划、矩阵、路网包、路径或求解。路径输入的 `networkArtifact` 使用拥有路由图产物的任务 ID，不接受客户端文件路径。`DELETE /v1/tasks/{id}` 清理终态任务，仍检查依赖、场景及执行审计引用。
- `GET /v1/road-packages`、`/{id}`、`/{id}/versions` 管理不可变源包；`/catalog` 查看全球目录；`POST /prepare` 按区域准备，`/import` 导入服务端可见 PBF。`/{id}/enable`、`/{id}/update`、`/{id}/rebuild` 和 `DELETE /{id}` 管理启用、更新、校验及未引用包清理。
- 更新先检查 ETag / Last-Modified，命中时直接复用，GET 也支持 304；`/prepare` 和 `/{id}/update` 可传 `forceDownload: true` 强制获取。`/{id}/rebuild` 校验已有本地源文件，不访问上游；工作台“按场景构图”提交矩阵准备任务，仅处理该场景所需范围。
- 场景摘要返回 `matrixReady` 和独立的 `geo` 状态。直接创建求解任务时矩阵不完整返回 `409 matrix_not_ready`；路网获取失败后，可以先显式生成并保存完整 FALLBACK 矩阵，再求解。

SQLite schema 6 在 schema 5 基础上增加底图包和显示引用。原有表继续保存通用任务、求解归属、产物和路网包。历史求解记录原位保留，升级不改写历史请求、方案、分数、曲线或执行引用。升级前使用数据库副本验证，回滚旧服务应恢复升级前备份。路网和矩阵实现说明见 [geo-tasks.md](geo-tasks.md)。

- `POST /v1/scenarios` 接收 `SaveScenarioRequest`：`scenarioId`、`name`、`expectedVersion`、`request`、可选 `display`。首次创建 expectedVersion 为 0；后续保存须等于最新版本，否则返回 409。每次保存创建新版本。展示名称、地址和备注按稳定 ID 存在 `display`，不进入内核；省略沿用上一版本，显式空消息清除，最大 1 MiB。
- `GET /v1/scenarios/{id}?version=N` 读取指定版本，省略 version 取最新。`GET /v1/scenarios?page_size=50&after_id=...` 分页列出最新版本摘要。
- `GET /v1/scenarios/{id}/metadata?version=N` 只读场景元数据，不加载问题和矩阵；工作台轮询版本，仅在版本变化时读取完整输入。
- 场景列表、元数据和完整读取统一提供 `Scenario.summary`：该版本的业务工单数和资源数，以及实时状态、关联任务数、历史版本数。状态区分待生成方案、已有方案、排队、求解、执行、已结束和待重新规划，不以浏览器是否打开过场景判断。旧数据首次启动时补存数量摘要，不改变业务版本、保存时间或请求。`savedUnixMs` 仍表示最近保存业务数据的时间。
- `DELETE /v1/scenarios/{id}?expected_version=N` 使用正数最新业务版本防止误删更新，一次删除全部版本、关联终态任务及道路折线，并释放存储额度。存在运行/排队/停止中的任务、执行记录（包含已结束执行）或外部保留引用时返回 409；版本过期返回 409，不存在返回 404。操作不会自动停止任务或删除执行审计。
- `POST /v1/jobs` 可以提交内联 `request`，或 `scenario: {scenarioId, version}`；两者只能选一个，场景版本必须显式给出正数。可提供 `options` 整体替换求解参数，无需制造业务版本。任务保存独立输入快照，后续编辑不影响已提交任务。
- `POST /v1/jobs/{id}/continue` 从终态结果热启动，问题保持不变，可整体替换 options；要求当前业务版本未变化，执行任务还要求执行事实和生效计划基准一致。返回新的排队任务，`kind=OPTIMIZATION`，保留 `sourceJobId`。
- `POST /v1/jobs/{id}/adjust` 接收 `solution`、`actor`、`reason`，下标对应来源任务冻结输入（含辅助装卸访问）。同步完成结构校验与全量评分，保存 `MANUAL_ADJUSTMENT` 终态候选；不自动采用，重启后也保持人工确认。采用时仍独立核查执行、载荷、容量、可达性及承诺。
- 普通 SubmitJob 的 kind 只接受 `PLANNING` 与 `REPLANNING`；其他用途由对应端点生成。Job 记录用途、来源任务及人工调整原因。
- `GET /v1/jobs?page_size=50&before_id=...&scenario_id=...` 按 ID 降序分页查询，返回轻量元数据，不加载矩阵和完整结果。page_size 上限为 100，返回值给出下一页游标。
- `GET /v1/jobs/{id}` 查询元数据；`POST /v1/jobs/{id}/cancel` 幂等取消；`DELETE /v1/jobs/{id}` 与 `DELETE /v1/tasks/{numericId}` 统一删除终态规划记录。成功清理输入、结果、趋势、道路折线及专属终态规划流程，释放相应额度；场景数据、共享包和已提交场景矩阵的来源图保留。运行中、曾被采用／关联执行审计、被派生方案或外部任务引用时返回 409，整个删除事务回滚。
- `GET /v1/jobs/{id}/trend` 返回 `JobTrend`：持久化的求解采样、采样间隔、历史缺失标记及当前任务 revision。首次有效进度、约每秒的进度、终态真实分数均保留，超过 2048 点时保留首尾并抽稀历史、增大采样间隔。曲线不会因浏览器刷新、切换场景或服务重启而丢失；终态任务无需重复读取。旧任务没有早期数据时 `partialHistory=true`，只返回已保存的真实末次记录，不伪造早期轨迹。
- 趋势载荷计入存储字节额度，随任务和场景级联删除；不单独占用任务记录额度。额度紧张时停止追加采样并标记历史不完整，保存最终结果时优先释放本任务趋势空间。数据库升级至版本 4，新增 `job_trends`；升级前备份，回滚旧服务时恢复升级前数据库。
- `GET /v1/jobs/{id}/request` 下载归档输入；`/result` 在 `hasResult=true` 后可用，否则 409。
- `GET /healthz` 返回线程额度、运行数、排队数与准入上限；停止或存储不可用时返回 503。

幂等键绑定规范化后的求解 protobuf、用途与场景版本引用：相同内容重复提交返回原任务，内容不同返回 409。归档的是服务实际交给 Worker 的输入字节。普通终态任务删除同时释放幂等键；实时重排任务、历史中曾被采用的静态任务、仍被派生任务引用的来源任务不能删除。场景和执行历史保留，自动归档/清理策略尚未提供，达到记录或字节上限后返回容量错误，需备份后调整配置或轮换运营数据目录。

`POST /v1/scenarios/import` 接收 `ImportScenarioRequest`，`requestBytes` 为原生 `SolveRequest` protobuf 的 base64；工作台可上传 `.pb` 或原生 ProtoJSON `.json`。历史 v1 文件先在 CLI 转换，不在服务增加兼容模式。

## 实时运营接口

| 路径 | 消息及行为 |
|---|---|
| `POST /v1/operations/start` | `StartOperationRequest`：显式业务版本、起始秒数、策略；默认自动重排、人工采用、LIVE 时钟。可携带 `initialJobId` 原子校验、采用静态方案并开始执行 |
| `GET /v1/operations/{id}?revision=N` | 当前执行快照；正数 N 读取历史快照，不改变现实 |
| `GET /v1/operations/{id}/history?page_size=N&before_revision=N` | 按修订降序分页返回事件摘要及下一页游标，含时间、类型、操作人、关联对象和业务/计划版本 |
| `POST /v1/operations/{id}/events` | `ExecutionEvent`：事件 ID、预期修订、发生时间及车辆/工单/货物更新，原子持久化 |
| `POST /v1/operations/{id}/configure` | `ConfigureOperationRequest`：整体替换策略及路线/时间承诺；时钟模式不可改变 |
| `POST /v1/operations/{id}/replan` | `ReplanRequest`：由最新事实构造剩余问题，options 仅影响本任务 |
| `POST /v1/operations/{id}/adopt` | `AdoptPlanRequest`：修订与计划版本双重检查，重新评分，原子发布 |
| `POST /v1/operations/{id}/ack` | `AcknowledgePlanRequest`：车辆确认精确计划版本 |
| `POST /v1/operations/{id}/close` | `CloseOperationRequest`：没有在途/服务工单或车载货物后关闭，释放资源，保留历史 |

实时实体需要稳定唯一的非空 ID；每辆实际车辆使用唯一非空 `resourceId`，同一资源不能同时属于两个运营场景。每个运营场景只允许一个活动 Agent 表示同一实际资源，多天同车的静态 Agent 列表须先选取本次运营的资源视图。`at` 是相对 Horizon 的秒，LIVE 随系统时间推进，REPLAY 只随事件推进。

静态方案开始执行时以起始状态重算排程；配送所需装货访问进入计划，不产生虚假的实际装货、出发或完成记录。校验失败不保留半完成的执行记录或资源占用。PublishedPlan 的 PlanRoute 新增可选 startTime/endTime/travelTime/travelDistance，来自采用时的执行投影与评分；旧计划缺失时不伪造回程或统计。

事件、配置和采用都带 `expectedRevision`。只有相同事件 ID **及相同完整消息**才返回首次结果；复用 ID 改内容返回 409。处理响应丢失时原样重试；收到版本冲突后重新读取状态，核对操作再生成新事件 ID。

车辆对同一当前计划的重复确认直接返回当前状态，允许使用首次确认前的修订号重试；旧计划确认始终返回冲突。

普通静态提交先做尺寸、规模、存在性、选项和额度校验，完整索引、矩阵、约束与初解校验在隔离子进程编译时进行；HTTP 202 表示已接收，之后仍可能以 `invalid_request` 失败。运营启动、编辑、投影和采用还会在应用层验证业务/执行边界。上限为 1000 业务单 / 200 车，投影产生的装卸辅助站另允许计入，总节点上限 2200；不是扩大业务任务规模。

## 调度、进度与恢复

队列持久化为元数据与输入记录，不为每个排队任务分配线程或保留解码后的矩阵。提交时在有界 HTTP 处理额度内解码并归档，取得执行额度后才重新加载输入、启动执行线程并调用同一 Supervisor。单一调度器按 FIFO 申请完整线程额度，且受运行进程数和内存准入限制；队首大任务等待时不让后面小任务插队。子进程与管道线程回收后才释放应用层额度。

`GET /v1/jobs/{id}/events` 提供 SSE，事件类型 `snapshot`，数据为完整 Job 元数据，事件 ID 为 `job_id:revision`。内存只保留每个任务的最新快照，慢订阅者会跳过中间心跳。重连时发送最新快照，不重放全部历史；`Last-Event-ID` 必须属于同一任务且不能超前。终态快照后关闭连接；客户端断连不取消求解。进度包含最佳分数与统计，不包含实时最佳路线。

任务状态包括 QUEUED、RUNNING、CANCELLING、SUCCEEDED、CANCELLED、FAILED、INTERRUPTED。排队取消没有结果；搜索中协作取消通常带最佳结果，始终以 `hasResult` 判断。强杀、协议失败、非法输入或结果无法归档会进入 FAILED，并给出原因，不伪造结果。求解恰好先于取消正常完成时可以是 SUCCEEDED。

第一版将场景元数据、版本输入、任务输入与结果全部存入 SQLite（WAL + FULL 同步），通过事务保证提交与结果状态一致；相比原先“SQLite + 独立文件”的方案，这避免第一版维护跨文件提交协议。未来大归档可以迁为内容寻址文件，不改变应用契约。逻辑字节限额包含场景展示元数据，但不包含任务元数据、索引、WAL 与空闲页；删除任务释放逻辑额度，但不会立即缩小数据库文件。数据目录需在本机磁盘，不能多个服务共享同一 WAL 数据库。

重启恢复未开始的队列，原 RUNNING/CANCELLING 标记 INTERRUPTED，保留旧进度供诊断。静态任务不自动重试；开启自动重排的运营场景保留需求并以最新事实创建新任务，不假装恢复搜索进度。持久化结果与采用之间崩溃时，恢复流程可重新检查自动采用。新线程额度低于旧任务要求时任务明确失败，不永久堵塞队列。

服务接收 Ctrl+C（Linux 另有 SIGTERM）后停止准入，保留排队项，取消运行项并回收子进程；HTTP 排空最多 15 秒。数据库不可用时停止继续派发。访问令牌、数据库与业务报告都不进入 Git；合成集成测试目录在 `corpus/` 创建并清理。

## 当前验证范围

测试覆盖场景版本冲突、输入快照、幂等键、并发额度、FIFO 与队列取消、真实子进程结果、错误后继续工作、模拟遗留运行状态的重启恢复、目录互斥、内存/存储准入、ProtoJSON 的大整数、SSE 重连与断连、鉴权/同源/请求尺寸，以及慢响应和已断连后台操作的额度保持。另有真实 TCP HTTP 监听测试，验证二进制场景与任务提交、结果下载及正常关闭。运行 `cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings`。本阶段验证环境为 Windows；新增服务器的 Linux 信号处理尚未在 Linux 实机验证。
