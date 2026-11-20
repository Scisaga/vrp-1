"""Copy and catalogue local vrp-0 fixtures; all generated material stays in corpus.

This is a developer tool, not a production importer or an execution-event adapter.
Original bytes are preserved. File names alone never establish a golden result.
"""

import argparse
from collections import Counter
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess


def plan(value):
    if isinstance(value, dict):
        if isinstance(value.get("tickets"), list) and isinstance(value.get("agents"), list):
            return value
        for key in ("plan", "scenario"):
            found = plan(value.get(key))
            if found is not None:
                return found
    return None


def scenario(value):
    if not isinstance(value, dict):
        return None
    if isinstance(value.get("plan"), dict) and any(
        key in value for key in ("planning_date", "matrix_build", "poi_build")
    ):
        return value
    return scenario(value.get("scenario"))


def describe(data, suffix):
    if suffix != ".json":
        return {"kind": "support"}, None
    try:
        value = json.loads(data.decode("utf-8-sig"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        # Do not echo source lines, which may contain customer information.
        return {"kind": "invalid", "parse_error": type(error).__name__}, None
    problem = plan(value)
    if problem is None:
        return {"kind": "support"}, value
    kind = "snapshot"
    if scenario(value) is not None:
        kind = "request"
    elif problem.get("score") is not None or any(
        key in value for key in ("scenario_id", "solver_time", "solve_time", "status")
    ):
        kind = "result"
    agents = [a for a in problem["agents"] if isinstance(a, dict) and not a.get("virtual")]
    return {
        "kind": kind,
        "tickets": len(problem["tickets"]),
        "agents": len(agents),
        "within_target": len(problem["tickets"]) <= 1000 and len(agents) <= 200,
    }, value


def entity_ids(value, entity):
    problem = plan(value)
    if problem is None:
        return None
    ids = []
    for item in problem[entity]:
        if not isinstance(item, dict) or item.get("id") is None:
            return None
        if entity == "agents" and item.get("virtual"):
            continue
        ids.append(str(item["id"]))
    return set(ids) if len(set(ids)) == len(ids) else None


def result_candidates(request, records, values):
    """Conservative naming + entity-set matches, still candidates, not ground truth."""
    candidates = []
    req_value = values[request["sha256"]]
    tickets = entity_ids(req_value, "tickets")
    agents = entity_ids(req_value, "agents")
    if tickets is None or agents is None:
        return candidates
    for result in records:
        if result["kind"] != "result":
            continue
        matches = []
        for left in request["origins"]:
            rp = Path(left["relative"])
            for right in result["origins"]:
                sp = Path(right["relative"])
                if left["root"] != right["root"] or rp.parent != sp.parent:
                    continue
                explicit_name = (
                    (rp.name == "scenario_request.json" and sp.name == "solver_response.json")
                    or sp.name.startswith(rp.stem + "_rp")
                    or sp.name.startswith("ticket_rp_" + rp.name + "_")
                )
                if explicit_name:
                    matches.append([left["relative"], right["relative"]])
        if not matches:
            continue
        result_value = values[result["sha256"]]
        if tickets != entity_ids(result_value, "tickets") or agents != entity_ids(result_value, "agents"):
            continue
        candidates.append({
            "path": result["path"],
            "basis": "same directory, explicit name relation, identical ticket and nonvirtual agent IDs",
            "sources": matches,
            "status": "candidate_not_golden",
        })
    return candidates


def run_eval(binary, output, case_id, arguments):
    report = output / "reports" / case_id
    report.mkdir(parents=True, exist_ok=True)
    try:
        run = subprocess.run(
            [str(binary), "eval", *map(str, arguments)],
            capture_output=True, timeout=120, check=False,
        )
        (report / "stdout.txt").write_bytes(run.stdout)
        (report / "stderr.txt").write_bytes(run.stderr)
        return {"status": "passed" if run.returncode == 0 else "failed", "exit_code": run.returncode,
                "report": report.relative_to(output).as_posix()}
    except subprocess.TimeoutExpired:
        return {"status": "timeout", "report": report.relative_to(output).as_posix()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, action="append", required=True,
                        help="vrp-0 checkout; reads only scenarios/ and src/test/resources/")
    parser.add_argument("--output", type=Path, default=Path("corpus"))
    parser.add_argument("--vrp", type=Path, help="optional built vrp executable for import/compile/oracle audits")
    args = parser.parse_args()
    output = args.output.resolve()
    binary = args.vrp.resolve() if args.vrp else None
    if binary and not binary.is_file():
        parser.error("--vrp executable does not exist")
    roots = [root.resolve(strict=True) for root in args.source]
    if any(output == root or output.is_relative_to(root) or root.is_relative_to(output) for root in roots):
        parser.error("source and output directories must not contain one another")
    if (output / "index.json").exists():
        parser.error("index.json already exists; use a new output directory to preserve the previous audit")
    output.mkdir(parents=True, exist_ok=True)
    files, values, sources = {}, {}, []
    for root in roots:
        head = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"],
                              capture_output=True, text=True, check=False)
        sources.append({"path": str(root), "git_head": head.stdout.strip() if head.returncode == 0 else None})
        for sub in ("scenarios", "src/test/resources"):
            for source in sorted((root / sub).rglob("*")):
                if not source.is_file() or source.is_symlink() or source.suffix.lower() not in (".json", ".csv", ".xlsx"):
                    continue
                if not source.resolve().is_relative_to(root):
                    raise ValueError("fixture resolves outside its source checkout")
                data = source.read_bytes()
                sha = hashlib.sha256(data).hexdigest()
                key = (sha, source.suffix.lower())
                if key not in files:
                    info, value = describe(data, source.suffix.lower())
                    relative = Path("raw") / info["kind"] / (sha + source.suffix.lower())
                    destination = output / relative
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    if destination.exists():
                        if destination.read_bytes() != data:
                            raise ValueError("existing content differs from its hash")
                    else:
                        with destination.open("xb") as stream:
                            stream.write(data)
                    files[key] = dict(info, sha256=sha, path=relative.as_posix(), bytes=len(data), origins=[])
                    values[sha] = value
                files[key]["origins"].append({"root": str(root), "relative": source.relative_to(root).as_posix()})
    records = sorted(files.values(), key=lambda record: record["path"])
    cases = []
    for record in records:
        if record["kind"] != "request":
            continue
        case = {
            "id": "case-" + record["sha256"][:12], "request": record["path"],
            "source": record["origins"][0], "tickets": record["tickets"], "agents": record["agents"],
            "within_target": record["within_target"],
            "result_candidates": result_candidates(record, records, values),
            "validation": {"status": "pending"} if record["within_target"] else {
                "status": "outside_target", "reason": "more than 1000 tickets or 200 nonvirtual agents",
            },
        }
        if binary and case["within_target"]:
            case["validation"] = run_eval(binary, output, case["id"], [output / case["request"]])
            if case["validation"]["status"] == "passed":
                for number, candidate in enumerate(case["result_candidates"]):
                    result = output / candidate["path"]
                    candidate["validation"] = run_eval(binary, output, case["id"] + f"-replay-{number}",
                        [output / case["request"], "--solution", result, "--matrix", result])
        cases.append(case)
    counts = dict(Counter(record["kind"] for record in records))
    validation = dict(Counter(case["validation"]["status"] for case in cases))
    selected = [case for case in cases if case["within_target"] and case["validation"]["status"] in ("passed", "pending")]
    index = {
        "format_version": 1, "created_utc": datetime.now(timezone.utc).isoformat(), "sources": sources,
        "counts": counts, "validation": validation, "files": records, "cases": cases,
        "notes": ["Private development corpus; do not commit.",
                  "Original bytes preserved; duplicate origins share one SHA-256 copy.",
                  "Result candidates are not golden results; input mutations and score-v1 differences require review.",
                  "Static snapshots are not evidence of real execution events.",
                  "Benchmark selection excludes malformed, ambiguous, out-of-target and failed-import records."],
    }
    (output / "index.json").write_text(json.dumps(index, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    (output / "bench.txt").write_text(
        "# Generated request-only selection; exclusions and replay pairs: index.json\n"
        + "".join(case["request"] + "\n" for case in selected), encoding="utf-8")
    lines = ["# 本地历史案例索引", "", "本目录全部为私有开发语料及报告，不提交 Git。原文件保持字节不变。", "",
             f"来源文件 {sum(len(r['origins']) for r in records)}，去重后 {len(records)}；分类：`{counts}`。",
             f"明确请求 {len(cases)}，校验状态：`{validation}`；默认基准清单 {len(selected)} 个请求。", "",
             "- `raw/`：按请求、结果、来源不明快照、辅助资料、损坏文件分类；SHA-256 文件名避免名称冲突。",
             "- `index.json`：原路径、来源版本、校验摘要、候选请求/结果配对及排除原因。",
             "- `bench.txt`：默认 bench 只运行此清单，避免把结果当输入或反复运行重复文件。",
             "- `reports/`：导入、编译、全量评分及候选结果回放的标准输出和警告。",
             "- 损坏文件与不明快照完整保留；候选配对不证明原始求解输入完全相同。",
             "- 静态历史文件不能代替真实的出发、到达、服务、完工、装卸、改道事件日志。", "",
             "| 案例 | 单/车 | 导入校验 | 结果候选 | 来源 |", "|---|---|---|---|---|"]
    for case in cases:
        source = case["source"]["relative"].replace("|", "\\|")
        lines.append(f"| [{case['id']}]({case['request']}) | {case['tickets']}/{case['agents']} | "
                     f"{case['validation']['status']} | {len(case['result_candidates'])} | {source} |")
    (output / "README.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    replay = Counter(candidate.get("validation", {}).get("status", "pending")
                     for case in cases for candidate in case["result_candidates"])
    print(json.dumps({"source_files": sum(len(r["origins"]) for r in records), "unique_files": len(records),
                      "counts": counts, "request_validation": validation, "replay_validation": dict(replay),
                      "benchmark_requests": len(selected)}, ensure_ascii=True))


if __name__ == "__main__":
    main()
