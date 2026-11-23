# 用 WorkBuddy 在本机部署和使用 vrp-1

求解、场景数据和决策工作台在运行 WorkBuddy 的电脑上。服务和 MCP 使用同一个本机端口。

## 直接交给 WorkBuddy

在 WorkBuddy 新会话发送这段提示词。智能体需要能够在本机获取源码和运行命令。

```text
从 https://github.com/Scisaga/vrp-1 获取源码。
按 README 启动本机服务，用仓库自带的样例排程。
优先用 MCP，也可按 docs/server.md 调用 HTTP API。
返回已安排、未分配数量，以及路线图与甘特图链接。
```

公开版可使用 `scripts/workbench_fixture.py` 的 `workbench_scenario` 生成合成样例。用 `POST /v1/scenarios` 保存后，按实际场景版本调用 `POST /v1/jobs`。用 `GET /v1/jobs/{id}` 查询，任务成功且 `hasResult` 为 `true` 时，从 `/v1/jobs/{id}/result` 读取结果。在工作台打开 `http://127.0.0.1:8080/#scene/{场景ID}/plans/{任务ID}` 查看路线和甘特图。[公开版服务文档](https://github.com/Scisaga/vrp-1/blob/main/docs/server.md)

## 含 MCP 版本的完整部署提示词

```text
请在这台电脑上部署 vrp-1，接入本机 WorkBuddy，并跑通一次工单排程。

1. 先检查当前目录和已安装的 vrp-server，能复用就直接启动。需要构建时，使用包含本指南和 MCP 服务的源码，按 docs/agent-local-quickstart.md 操作。使用独立的数据目录 var/workbuddy-local 和环回地址 127.0.0.1:8080。已在运行的服务先检查端口、数据目录和 MCP 工具，避免重复启动。
2. 启用同端口 /mcp。按本页配置合并名为 psiai-local 的 MCP 连接，保留已有连接。遇到 WorkBuddy 要求在设置中刷新连接时，给出待粘贴配置和当前验证结果。
3. 有待规划的业务场景时先列出场景和版本，使用用户指定的场景。空库可按本页创建 12 单验收样例，不修改其他业务数据。
4. 通过 list_scenarios 确认场景，再调用 solve_plan。需要等结果时用 get_plan 查询同一个任务。直接返回分配结果和工具给出的 viewUrl，在 WorkBuddy 右侧打开路线与甘特图。
5. 执行中的业务变更先生成重排候选。用户明确确认采用某个候选后，再用该候选编号、评审时的执行修订和生效计划版本调用 adopt_plan。成功后回复生效版本，并打开工具返回的工作台链接。
6. 完成后告诉我本机 URL、数据目录、MCP 工具和验收结果。
```

## 构建和启动

已有支持 `/mcp` 的二进制时直接启动。源码构建需要支持 Rust Edition 2024 的 Rust 工具链和系统编译工具。在含本指南的仓库根目录运行以下命令。

```powershell
cargo build --locked --release -p vrp-server
```

从仓库根目录启动。下列 PowerShell 命令适合智能体在后台运行服务，再验证端口。端口变更时同步改绑定地址、`--mcp-origin` 和客户端 URL。

```powershell
$vrpExecutable = (Resolve-Path -LiteralPath '.\target\release\vrp-server.exe').Path
$vrpArguments = @('--bind','127.0.0.1:8080','--mcp-origin','http://127.0.0.1:8080',
                  '--data-dir','var/workbuddy-local','--geo-prefetch','false','--basemap-auto','false')
Start-Process -FilePath $vrpExecutable -ArgumentList $vrpArguments -WorkingDirectory (Get-Location).Path -WindowStyle Hidden
Invoke-RestMethod -Uri http://127.0.0.1:8080/healthz
```

如果服务仍在启动，等待片刻后再请求 `/healthz`。使用发行包时将可执行文件路径改为解压目录的 `vrp-server.exe`。Linux 源码构建后使用 `./target/release/vrp-server` 和同样的启动参数。重启时指向原数据目录。启用自动路网准备、离线底图或连接其他电脑时，按对应服务配置操作。[服务参数](server.md) · [离线底图](offline-basemaps.md)

## WorkBuddy 连接

在 MCP 设置合并以下配置。同一台电脑通过环回地址连接时可直接使用 HTTP，省去令牌和 OAuth。将链接打开模式设为 **Built-in**，便于在右侧打开工具返回的决策工作台。[跨电脑连接、鉴权与工具参数](workbuddy-mcp.md)

```json
{
  "mcpServers": {
    "psiai-local": {
      "type": "streamableHttp",
      "url": "http://127.0.0.1:8080/mcp"
    }
  }
}
```

## 用一个本地样例验收

业务场景可在工作台导入原生 `.json` 或 `.pb`。空库可以在仓库根目录用 Python 3 创建内置测试样例。样例有 12 张工单、3 个执行单元和预先计算的估算时距矩阵，地图显示示意路线。重复运行时复用同名场景。

```powershell
@'
import json
from urllib.error import HTTPError
from urllib.request import ProxyHandler, Request, build_opener
from scripts.workbench_fixture import workbench_scenario

base = "http://127.0.0.1:8080/v1/scenarios"
scene_id = "wb-local-demo"
http = build_opener(ProxyHandler({}))
try:
    with http.open(f"{base}/{scene_id}", timeout=5) as response:
        scene = json.load(response)["scenario"]
except HTTPError as exc:
    if exc.code != 404:
        raise
    request = Request(base, data=json.dumps(workbench_scenario(scene_id)).encode("utf-8"),
                      headers={"Content-Type": "application/json"})
    with http.open(request, timeout=10) as response:
        scene = json.load(response)
print(scene["id"], scene["version"], "matrixReady=", scene.get("matrixReady", False))
'@ | python -
```

在 WorkBuddy 发「用 psiai-local 给 `wb-local-demo` 排程，返回指派结果和工作台链接」。以看到 `12` 单已安排、`0` 单未分配并能在链接内点选甘特工单为验收。准备完整离线路网与任务道路折线后，右侧地图可显示沿道路的路线。[路网和折线](geo-tasks.md)
