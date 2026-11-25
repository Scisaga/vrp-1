# WorkBuddy 与 Codex 接入

vrp-server 可以在现有端口提供 Streamable HTTP MCP。MCP 与工作台共用场景、任务队列、求解、执行快照和道路折线。启动时用 `--mcp-origin` 启用 `/mcp`，该值同时是允许访问的 HTTP 来源和返回给客户端的工作台地址。

可先用通用提示词在本机启动服务、下载地图包并生成首个道路方案，再按本页接入 MCP。[智能体本地部署指引](agent-local-quickstart.md)

## Windows 启动

```powershell
cargo build --release -p vrp-server -p vrp-cli
.\target\release\vrp-server.exe --bind 127.0.0.1:8080 --data-dir var --mcp-origin http://127.0.0.1:8080 --total-threads 4
```

从另一台电脑连接时，使用客户端能访问的服务地址。在运行服务的电脑上将随机生成的测试令牌设置为 `VRP_API_TOKEN`，再用同一内网地址替换 `--bind` 和 `--mcp-origin`。同一 Windows 服务也可由 Codex 使用。若客户端要求 HTTPS，给该测试端点配置受信的内网 TLS 入口，`--mcp-origin` 写入口的 HTTPS 地址，反代保留 Host 和转发来源。

- **WorkBuddy** 在 MCP 配置中合并 [workbuddy-mcp.json](examples/workbuddy-mcp.json)。内网访问时改 URL，并在私有配置中填写真实测试令牌。本机环回可删去 headers。在 WorkBuddy 的 Settings → General → Link Open Mode 选择 Built-in，点击结果链接即可在右侧浏览器打开。新开的工作台页面在「访问设置」中填入同一令牌后会直接定位到链接指定的方案。右侧空间较窄时可拉宽面板或缩小页面显示比例。
- **Codex** 在设置中添加 Streamable HTTP 端点，或合并 [codex-mcp.toml](examples/codex-mcp.toml)。本机 CLI 可用 `codex mcp add psiai --url http://127.0.0.1:8080/mcp`。有令牌时使用环境变量注入。

已验证 Codex App Server 和 WorkBuddy 可连接同一个服务端点。启用 HTTP 代理时，将内网服务地址加入 Codex 进程的 `NO_PROXY`。

## 工具

| 工具 | 最小参数与行为 |
| --- | --- |
| `list_scenarios` | 可选 `afterId`。返回场景版本、规模、矩阵是否就绪及现有执行状态和修订号。 |
| `solve_plan` | 传 `scenarioId`、`expectedVersion`、`requestId`，或使用预配置场景并只传新的 `requestId`。已有矩阵直接求解，缺矩阵则使用现有路网规划流程。可传 `waitForResult: true` 让一次调用同时返回结果摘要与工作台链接。 |
| `replan` | `scenarioId`、`expectedRevision`、`requestId`，以及 `disabledAgentId` 或 `urgentTicket`。紧急服务单使用已有 `locationId`，含 `ticketId`、`durationSeconds`、`latestStartSeconds` 与可选 `skillsRequired`。时间从场景零点起计，单位秒。业务更新、执行事件与候选入队同一事务提交。可传 `waitForResult: true` 同时读取摘要及紧急单到场时间。要求执行中有生效计划并由人采用候选。 |
| `get_plan` | `taskId`，可选 `ticketId`、`includeAssignments`。返回求解、折线状态和可分屏打开的工作台 URL。终态结果附覆盖量、里程和搜索耗时。查询指定工单可获得工作单元与到场、服务时间。验分时可请求有序指派 ID。 |
| `adopt_plan` | 用户明确确认候选后提供 `scenarioId`、`jobId`、`expectedRevision`、`expectedPlanVersion`、`confirmed: true`、`actor`。可选 `reason`、`ticketId`。服务重新校验并原子发布，返回 `activeJobId`、`planVersion` 和工作台链接。有 `ticketId` 时从生效计划读取工单时间。 |

同一意图重试时复用原 `requestId` 和业务参数。`waitForResult` 只控制本次回复，可以在重试时改变。场景版本或执行修订发生冲突时先读取现状。道路折线使用与被冻结矩阵相符的路网产物，沿路路径不足时工作台给出提示。端到端耗时还包含排队、准备、几何生成和客户端操作。

确认采用时，从该候选的 `get_plan` 结果取 `operation.revision` 和 `operation.planVersion`，与用户所核对的候选一起提交。原确认重复发送时返回已生效的同一版本。状态发生冲突时，智能体应展示最新状态，请用户重新核对后确认。采用由已有执行服务检查硬约束、载荷、承诺锁和道路可达性。

## 已知场景的单次调用

场景、版本或执行修订已知时直接调用相应工具。默认异步返回任务号。为短时求解传入 `waitForResult: true` 后，服务订阅求解状态，至多等待 10 秒。返回已通过引擎全量评分的结果摘要时，可直接呈现链接和关键指标。道路折线另行生成，状态随工作台更新。若仍在排队、路网矩阵正在准备或超过等待时间，回复保留任务号，之后可用 `get_plan`。有序指派只在独立验分需要时读取，由采集脚本从同一任务取回。

交付沿路方案时，若 `geometryState` 为 `waiting`、`queued` 或 `running`，按原 `taskId` 继续查询。完成后回复实际道路段数 `roadLegs`、总段数 `totalLegs` 和覆盖状态，失败时给出 `geometryMessage`。需要用道路矩阵重新规划已有场景或同时准备离线底图时，使用[首跑命令](agent-local-quickstart.md#用一个本地样例验收)。

启动时加 `--mcp-default-scenario wb-initial@1` 可将初排绑定到该场景的第 1 版。发送「把今天的工单排一下」时，客户端给 `solve_plan` 生成新的 `requestId` 即可，省去读取场景列表和版本。使用该默认场景时，工具默认同时等待摘要和链接。响应携带提交、开始与完成时刻，便于把模型等待、引擎和结果展示分开计时。已有 `viewUrl` 时，可直接打开读取进度和结果。

```powershell
.\target\release\vrp-server.exe --bind 127.0.0.1:8080 --data-dir var --mcp-origin http://127.0.0.1:8080 --total-threads 8 --mcp-threads 8 --mcp-search-ms 6500 --mcp-search-strategy nearest --mcp-default-scenario wb-initial@1
```

MCP 默认 30 秒、最多 4 线程、`baseline`。部署参数可单独调整线程数和搜索预算，普通 HTTP 提交仍使用各自的请求参数。

工具参数中的业务标识按场景原值传入。中文场景的技能原值为「安装」时，`skillsRequired` 传 `["安装"]`。遇到未知技能值，服务先返回该场景的技能原值，业务变更与重排等校对后再提交。

## 右侧方案视图

MCP 返回 `/?view=panel#scene/<场景ID>/plans/<任务ID>` 或 `/?view=panel#scene/<场景ID>/operations/<候选ID>`。打开后直接展示该场景的方案指标、路线图与甘特图。重排视图可在该候选和原生效方案间切换。确认采用后重开相同链接即可看到生效计划。访问令牌沿用服务的现有鉴权。完整工作台也提供场景管理和人工采用操作。

## 480 单录制场景复现

试跑需要本地已有的 480 单场景，源数据目录应包含 SQLite、地图包和路网工件。将下例的 `var/workbuddy-source` 换成实际源目录，在副本上准备场景。所有试验输出放入本地 `target/workbuddy-demo/`。

```powershell
python scripts/workbuddy_demo.py copy --source-data var/workbuddy-source --data-dir target/workbuddy-demo/seed-data
.\target\release\vrp-server.exe --bind 127.0.0.1:8080 --mcp-origin http://127.0.0.1:8080 --data-dir target/workbuddy-demo/seed-data --total-threads 4 --geo-prefetch false --basemap-auto false
```

另开终端，准备初排场景、在途原计划和执行事实。

```powershell
python scripts/workbuddy_demo.py prepare --base-url http://127.0.0.1:8080 --fixtures target/workbuddy-demo/fixtures
```

停止准备服务后，用 `copy` 再克隆一份试跑数据目录。用相同参数启动试跑服务，`--data-dir` 指向新的副本。每次正式重排使用新副本或恢复同一个准备快照。

```powershell
python scripts/workbuddy_demo.py verify --base-url http://127.0.0.1:8080 --fixtures target/workbuddy-demo/fixtures --output target/workbuddy-demo/preflight
```

`verify` 发起真实初排和重排，保存冻结请求、实际指派和可复查的本机运行记录。正式对比使用准备好的业务输入及这次试跑冻结的重排问题，双方的指派用同一命令复核。

也可在隔离目录连续执行一遍本机预检。安装 Playwright 和对应 Chromium 后，`--capture` 额外保留工作台截图与浏览器测试录像。

```powershell
python scripts/workbuddy_demo.py local --source-data var/workbuddy-source --run-directory target/workbuddy-demo/local-qa --capture
```

在已准备好的独立试跑库上，可用以下参数验证 8 线程与单次调用。记录包含 MCP 首次回复、服务端入队与搜索、独立验分及道路折线就绪时间。

```powershell
python scripts/workbuddy_demo.py verify --base-url http://127.0.0.1:8080 --fixtures target/workbuddy-demo/fixtures --output target/workbuddy-demo/fast-preflight --mcp-threads 8 --mcp-search-ms 6500 --wait-for-result --pinned-scene
```

```powershell
.\target\release\vrp.exe eval-plan frozen.request.pb assignments.json --baseline baseline.assignments.json --urgent-ticket URGENT-01
```

指派文件格式为 `{"routes":[{"agentId":"工作单元ID","ticketIds":["按访问顺序排列的工单ID"]}]}`。初排可省略原方案和紧急单参数。验分只读，用既有全量评分器按冻结矩阵重算到场、时限和里程。重复指派、未知 ID 以及破坏锁定顺序会退出并报错。
