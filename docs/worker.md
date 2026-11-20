# 本地 Worker 进程隔离

`vrp-worker` 提供可嵌入宿主的 `Supervisor`、通用 `ProcessRunner` 和一次性子进程入口。CLI 可直接运行：

```bash
cargo run --release -p vrp-cli -- solve-isolated corpus/request.pb corpus/result.pb --preset fast --threads 2 --search alpha-kopt --k-opt-depth 5 --memory-mib 1024
```

输入也支持现有 v1 JSON 导入。结果路径必须尚不存在。算法选项与普通 `solve` 相同，隔离模式另有：

| 参数 | 默认 | 含义 |
|---|---:|---|
| `--memory-mib` | 1024 | 子进程内存额度，0 为显式不限制 |
| `--max-frame-mib` | 256 | 单条 IPC 消息最大字节数，解码前检查 |
| `--setup-timeout-ms` | 30000 | 请求传输、编译及解释阶段的监督时间 |
| `--cancel-grace-ms` | 2000 | 发出协作取消后，等待最佳结果并正常退出的宽限 |

库配置另有进度间隔（默认 250ms）、心跳超时（5 秒）和搜索超时宽限（5 秒）。宿主通过 `AtomicBool` 发起取消，通过 `solve_observed` 的回调接收进度；回调需及时返回，网络上报应由宿主异步处理。

## 生命周期

1. 解析小型求解参数并检查 IPC 长度，然后向共享 `Executor` 申请该任务的全部线程额度。排队期间不启动子进程。
2. 启动受信任的求解程序，建立操作系统资源限制。Windows 子进程在 Job Object 绑定完成之前不会收到请求。
3. stdin 发送 Start，之后可发送 Cancel；stdout 只传递 Progress、Result 或 Failure。协议都定义在 `worker.proto`，消息前加四字节小端长度，没有额外 JSON 契约。
4. 子进程在独立地址空间编译、搜索并用 oracle 生成解释。核心 `SearchMonitor` 只更新内存数据，专用输出线程发出进度，搜索线程不执行管道 I/O。
5. supervisor 监测编译/搜索/解释阶段、心跳和请求时限。DEEP 使用 `elapsed_ms - best_found_ms` 检查连续无改进时间，严格改善会延长可执行时间；不会把 300 秒误当总时限。
6. 取消或监督超时先发送 Cancel。子进程正常响应时返回已记录的最佳方案；超过宽限则终止进程，返回 `Killed` 错误。强制终止没有已确认的最终结果，不伪造部分方案。
7. 必须等子进程和输入/输出线程都回收后，才归还额度。崩溃、协议错误、内存限制、宿主回调异常展开均走清理路径。

父进程不在持有队列锁时执行 I/O。管道读写各使用专用线程，输出队列有界，即使子进程不读请求或不输出进度，监督循环也能触发回收。子进程发现 stdin EOF 时请求取消。

## 操作系统行为

- **Windows**：使用 Job Object，限制 Job 总提交内存并启用 `KILL_ON_JOB_CLOSE`；终止 Job 回收子进程及其继承该 Job 的后代。父进程退出时句柄关闭也触发清理。启动时使用 `CREATE_NO_WINDOW`。[Microsoft Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)
- **Linux**：独立进程组，强制回收时向组发送 SIGKILL；在 exec 前设置 `RLIMIT_AS`、`PR_SET_PDEATHSIG`，并关闭 core dump。正常退出时先用 `waitid(WNOWAIT)` 检测，再清理进程组，最后回收 PID，避免清理期间 PID 被复用。RLIMIT_AS 限制虚拟地址空间，与 Windows 提交内存计量不同；不是 RSS 限额。[Linux 资源限制](https://man7.org/linux/man-pages/man2/getrlimit.2.html)、[父进程死亡信号](https://man7.org/linux/man-pages/man2/PR_SET_PDEATHSIG.2const.html)

该入口用于隔离受信任的求解代码故障，不是运行恶意代码的安全沙箱。内存额度不包含 supervisor 持有的原始请求与结果；调用方仍需有界提交、配置宿主或容器总内存。多个独立 supervisor 进程须由部署层切分机器配额。

## 路网计算子进程

服务以同一可执行文件的 `geo-child` 模式读取 PBF、校验源包、构建路由图及生成矩阵和折线。通用隔离协议为 `ProcessCommand` / `ProcessEvent`，负载使用 `GeoRunRequest` / `GeoRunResponse`；全部消息定义在 `worker.proto`。大文件放在受管理的任务目录，IPC 传递路径、大小及 SHA-256 校验信息。

通用任务由服务调度器预留 CPU 和内存，调用 `ProcessRunner::run_reserved` 后不再次申请计算额度。求解任务可通过 `solve_reserved_observed` 使用已有 `ThreadPermit`。等待地图或依赖的父任务不持有计算额度。通用进度、心跳、取消和强制回收复用求解隔离机制，构图准备时限独立于搜索时限。

CLI 的 `solve`、`solve-isolated` 先显式固化旧输入的估算矩阵，再通过矩阵完整性检查。求解子进程独立复核完整性；库层 compile、eval 和基准回放保留原有估算语义。

## 当前范围

本地子进程执行支持进度、协作取消、强制回收和资源限制。`worker.proto` 定义本地求解的 `ChildCommand` / `ChildEvent` 以及通用计算的 `ProcessCommand` / `ProcessEvent`。

独立服务 `vrp-server` 已提供本机任务提交、排队、查询和取消，直接调用本 Supervisor，并把 Ctrl+C / Linux SIGTERM 转换成协作取消与排空。原 `vrp-cli` 仍未接入控制台信号转换。服务运行说明见 [server.md](server.md)，前端与发行方案见 [standalone.md](standalone.md)。

故障测试覆盖：正常结果与评分复核、不同 PID 的实时进度、取消返回最佳方案、DEEP 无改进停止、子进程异常退出、长期无响应、内存限制、帧上限、多个隔离任务共享额度，以及故障后继续执行新任务。

2026-10-29 已在 Windows 与 WSL Ubuntu（Linux，Rust 1.99.1）运行工作区测试；Windows 同时验证 release 构建。上述验证使用合成数据。
