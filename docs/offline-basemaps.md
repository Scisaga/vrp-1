# 离线底图包

底图用于显示，不进入矩阵、评分、冻结输入或执行事实。计算路网仍由 `vrp-geo` 处理；底图应用组件为 `vrp-basemap`。消息与接口以 `proto/vrp/maps/v1/maps.proto` 和 app/worker 协议为准。

## 使用与默认值

首次打开已保存场景地图时，先检查本地包；没有覆盖时显式提交后台准备任务。默认提取 Protomaps v4、原生 0–14 级、场景范围外扩 10 公里。拖动、缩放不扩大下载范围。15 级只通过“准备详细底图”或导入获得，显示缩放仍可到 19。

本地包优先；未准备好或超出覆盖时使用现有在线底图，在线不可用时保留路线、工单和示意图。底图不作为求解或采用的依赖。自有 `--map-config` 或禁用设置优先，不自动覆盖。

自动下载预算 500 MiB，底图总磁盘额度 10 GiB；临时峰值纳入准入。每个准备子进程占 1 CPU、512 MiB，共享宿主额度。下载、元数据及目录访问均计入传输量。超额明确失败，不静默降低精度。缓存命中不访问上游。更新显式生成新版本，不改历史包。

支持从服务端可见文件/目录导入 PMTiles v3（Protomaps v4 兼容 MVT 或 PNG/JPEG/WebP 栅格），复制并验证后发布。目录内使用 archive.pmtiles；不支持浏览器大文件上传、MBTiles 转换或从 PBF 制瓦片。

## 接口与生命周期

- `GET /v1/basemap-packages`、`/{id}`：包、范围、精度、字节数、来源、版本及引用。
- `POST /v1/basemap-packages/prepare`、`/import`：返回通用任务；重复范围复用已有任务，GET 不创建任务。
- `POST /v1/basemap-packages/estimate`：提交只读取目录的隔离预估任务。手动准备先展示范围、目标级别、预计新增传输及磁盘峰值，确认后以 `estimate_task_id` 固定该来源快照；预估失败不启动瓦片下载。
- `POST /v1/basemap-packages/{id}/enable`、`/update`：预期版本校验；更新保存独立包。
- `DELETE /v1/basemap-packages/{id}`：仅清理未被显示上下文或活动任务引用的包。
- `GET /v1/basemaps/selection`：明确引用场景版本、冻结任务或执行状态之一，返回已选包、覆盖与准备状态。
- `POST /v1/basemaps/selection`：以预期包 ID 显式绑定显示引用，读取不写入数据库。
- `GET/HEAD /v1/basemaps/{id}/archive.pmtiles`：单段 Range、206/416、ETag、条件读取。
- `/v1/basemap-assets/{version}/...`：随程序提供的样式、中文/拉丁字形和图标。

准备使用统一 `BASEMAP_PREPARE` 任务、取消和重试机制，源快照固定后才提取。仅验证成功的不可变文件发布为可用包；半成品不是覆盖。目录、区间、截断和 ETag 校验失败终止准备，禁止退化成整份全球下载。

SQLite schema 6 添加包与显示引用表，不改变旧场景、任务和执行快照。升级不为历史场景批量下载；回滚恢复升级前数据库备份。

相同范围和精度的并发准备复用同一固定源任务；等待该包的显示上下文分别登记，发布时一并绑定。源快照固定后不因刷新或重试而更换。

## 访问与验收

本机回环直连免 token，非回环及转发请求继续鉴权。只有同源受控地图资源携带当前内存 token，外部地图绝不携带。文件接口不接受任意路径；地图读取使用独立有界名额，业务 API 保持 no-store，不覆盖不可变底图的私有缓存头。

样式、字形和图标固定版本随程序提供并保留第三方声明；区域包和私有报告不进 Git。测试使用本地合成 PMTiles 与 Range 服务，不请求公共瓦片。验收要求准备后断外网、重新打开浏览器仍显示中文底图，浏览器零外网请求；默认不含 15 级，手动升级不损坏已有 14 级包。

## 构建与验证

固定资源清单位于 `web/vendor/basemap/manifest.json`。`VRP Sans` 从固定版本的 Noto Sans CJK SC Regular 生成，随包提供中文与拉丁字形（BMP），无需浏览器操作系统安装中文字体。字形为 gzip PBF，服务器使用正确的 Content-Encoding 返回；字体和图标许可证保留在资源目录。

```powershell
python scripts/vendor_basemap.py --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python scripts/smoke_offline_basemap.py --server target/basemap-release/release/vrp-server.exe
powershell -File scripts/package-windows.ps1 -BuildTargetDirectory target/basemap-release
```

重建固定字体仅在开发环境需要 Python 与 Node，运行服务不需要它们。独立构建目录和临时验收数据分别位于 target 与 corpus，被 Git 忽略。验收覆盖本地多级 PMTiles 目录、重复载荷、14/15 级、冷启动零外网请求、中文实际绘制、同源完整地图资源鉴权和外部 token 隔离。

升级前记录可执行文件、数据目录和启动参数，通过 SQLite 备份 API 生成一致副本。在副本上升级 schema 6 并比较场景、任务、分数和执行历史；正式切换再次备份，继续使用原数据目录、线程额度、路网与访问配置。回滚时恢复旧程序与对应 schema 5 数据库，不用旧程序直接打开 schema 6 数据库。
