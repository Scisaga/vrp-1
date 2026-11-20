# vrp-1 AGENTS

vrp-1 是 PsiAI 的 VRP 引擎（Rust），目标包含独立部署服务与前端；其中 `vrp-core` 是纯计算内核，无外部 I/O。开始任务前先读 [docs/architecture.md](docs/architecture.md)；涉及评分时读 [docs/constraints-v2.md](docs/constraints-v2.md)。

## 不可破坏的约束

- 契约只有 `proto/` 下的 `.proto` 文件。改契约时同步更新相关文档。
- 评分语义以 `docs/constraints-v2.md` 为准。先改文档，再改实现；oracle 和增量评分要同时改，并用测试证明二者一致。
- `vrp-core` 只依赖标准库，不做 I/O、不用 async、不持有全局状态。
- 不做 vrp-0 的兼容模式。`vrp-import-v1` 只用于基准测试和历史回放。
- 私有业务语料默认放在项目根目录的 `corpus/`，也可通过 `--corpus` 或 `VRP_CORPUS_DIR` 指定目录。目录中的语料、导出物及包含客户名称或地址的报告不得提交；只提交用于保留目录的 `.gitkeep`。

## 常用命令

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run --release -p vrp-server -- --bind 127.0.0.1:8080 --data-dir var --cpu-multiplier 1.8
cargo run --release -p vrp-cli -- bench          # 从项目根目录执行，默认读取 ./corpus
cargo run --release -p vrp-cli -- solve <请求.pb或v1.json> <结果.pb> --preset fast --threads 4
cargo run --release -p vrp-cli -- solve-isolated <请求.pb或v1.json> <结果.pb> --search alpha-kopt --threads 2 --memory-mib 1024
cargo run --release -p vrp-cli -- eval <v1 请求.json> [--solution <v1 结果.json>] [--matrix <v1 结果.json>] [--weight <约束名>=<权重>]... [--show <约束名>]
```
