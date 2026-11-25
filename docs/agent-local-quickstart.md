# 智能体本地部署指引

将下方提示词交给能在本机获取源码并运行命令的智能体。

## 通用部署提示词

```text
请在本机部署 vrp-1（https://github.com/Scisaga/vrp-1），按 docs/agent-local-quickstart.md 启动服务并执行首跑命令。
使用我指定的场景，未指定时用内置样例。
等待路网包、离线底图和沿路方案就绪，返回已安排数、未分配数、实际道路段数与总段数，以及工作台链接。
任务失败时给出任务号和原因。
```

## 构建和启动

已有本地 `vrp-server` 时先检查 `/healthz`，可用则沿用其地址和数据目录。已有兼容的二进制时可直接启动。需要构建时，在仓库根目录使用支持 Rust Edition 2024 的工具链和系统编译工具运行以下命令。

```powershell
cargo build --locked --release -p vrp-server
```

在仓库根目录用 PowerShell 启动服务并检查健康状态。

```powershell
$vrpExecutable = (Resolve-Path -LiteralPath '.\target\release\vrp-server.exe').Path
$vrpArguments = @('--bind','127.0.0.1:8080',
                  '--data-dir','var/workbuddy-local','--geo-prefetch','true','--basemap-auto','true')
Start-Process -FilePath $vrpExecutable -ArgumentList $vrpArguments -WorkingDirectory (Get-Location).Path -WindowStyle Hidden
Invoke-RestMethod -Uri http://127.0.0.1:8080/healthz
```

如果服务仍在启动，稍后再请求 `/healthz`。Linux 上可用 `./target/release/vrp-server` 及相同的启动参数。重启时指向原数据目录。[服务参数](server.md)

## 用一个本地样例验收

服务就绪后，从仓库根目录用 Python 3 运行首跑命令。默认创建或复用 12 单样例。

```powershell
python scripts/workbuddy_demo.py quickstart
```

使用自己的场景时，先导入原生 `.json` 或 `.pb`，再追加 `--scenario-id <场景ID>`。服务地址有变化时传入 `--base-url <服务地址>`。命令复用已有地图包或下载缺包，生成道路矩阵、求解并等待道路折线和离线底图。[路网和折线](geo-tasks.md) · [离线底图](offline-basemaps.md)

以命令成功退出为准。输出应有路网包和底图包 ID，且实际道路段数 `roadLegs` 大于 0。用 `totalLegs` 核对覆盖，打开 `viewUrl` 查看路线和甘特图。命令默认等待 600 秒，超时后用原任务号继续查询。

## 可选 MCP 接入

启动时加 `--mcp-origin http://127.0.0.1:8080`，再将 `http://127.0.0.1:8080/mcp` 配置为智能体的 Streamable HTTP 端点。[客户端配置、鉴权和工具用法](workbuddy-mcp.md)
