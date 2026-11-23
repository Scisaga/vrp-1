"""Prepare and verify the 480-order WorkBuddy trial using the existing Shanghai fixture."""
import argparse
from contextlib import closing, contextmanager
import hashlib
import json
import os
from pathlib import Path
import shutil
import secrets
import socket
import sqlite3
import subprocess
import time
from urllib.error import HTTPError
from urllib.parse import urlencode
from urllib.request import ProxyHandler, Request, build_opener


ROOT = Path(__file__).resolve().parents[1]
BIN_SUFFIX = ".exe" if os.name == "nt" else ""
INITIAL = "wb-initial"
REPLAN = "wb-replan"
END_STATES = {"SUCCEEDED", "FAILED", "CANCELLED", "INTERRUPTED"}
PROJECT_OPTIONS = {"threads": 4, "timeLimitMs": "30000", "seed": "42", "searchStrategy": 1}


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def clone(source, destination):
    source, destination = source.resolve(), destination.resolve()
    if not (source / "tasks.sqlite").is_file():
        raise ValueError(f"No prepared database at {source}")
    if source == destination or source in destination.parents or destination.exists():
        raise ValueError("Use a new directory outside the source database")
    destination.mkdir(parents=True)
    for folder in ("task-artifacts", "road-packages", "basemap-packages"):
        if (source / folder).is_dir():
            shutil.copytree(source / folder, destination / folder)
    with (closing(sqlite3.connect((source / "tasks.sqlite").as_uri() + "?mode=ro", uri=True)) as original,
          closing(sqlite3.connect(destination / "tasks.sqlite")) as replica):
        with replica:
            original.backup(replica)
            for table in ("task_artifacts", "road_packages", "basemap_packages"):
                for row, path in replica.execute(f"SELECT rowid,path FROM {table}").fetchall():
                    try:
                        new = destination / Path(path).resolve().relative_to(source)
                    except ValueError:
                        continue
                    replica.execute(f"UPDATE {table} SET path=? WHERE rowid=?", (str(new), row))
    print(f"Prepared isolated data directory {destination}", flush=True)


class Client:
    def __init__(self, base):
        self.base = base.rstrip("/")
        self.token = os.environ.get("VRP_API_TOKEN")
        self.opener = build_opener(ProxyHandler({}))
        self.ident = 0
        self.session = None
        self.protocol = None

    def http(self, path, payload=None, accept="application/json", extra=None, method=None):
        headers = {"Accept": accept}
        data = None
        if payload is not None:
            data = json.dumps(payload).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if self.token:
            headers["Authorization"] = "Bearer " + self.token
        headers.update(extra or {})
        request = Request(self.base + path, data=data, headers=headers, method=method)
        with self.opener.open(request, timeout=25) as response:
            return response.read(), response.headers

    def api(self, path, payload=None):
        try:
            return json.loads(self.http(path, payload)[0])
        except HTTPError as error:
            raise RuntimeError(f"{path}: {error.read().decode('utf-8', errors='replace')}") from error

    def rpc(self, method, params, notification=False):
        self.ident += 1
        message = {"jsonrpc": "2.0", "method": method, "params": params}
        if not notification:
            message["id"] = self.ident
        headers = {}
        if self.protocol:
            headers["MCP-Protocol-Version"] = self.protocol
        if self.session:
            headers["MCP-Session-Id"] = self.session
        data, response = self.http("/mcp", message, "application/json, text/event-stream", headers)
        if notification:
            return None
        if response.get_content_type() == "text/event-stream":
            candidates = [json.loads(line[6:]) for line in data.decode().splitlines() if line.startswith("data: ") and line[6:].strip()]
            value = next(row for row in candidates if row.get("id") == self.ident)
        else:
            value = json.loads(data)
        if "error" in value:
            raise RuntimeError(value["error"])
        self.session = response.get("MCP-Session-Id") or self.session
        return value["result"]

    def connect(self):
        result = self.rpc("initialize", {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "workbuddy-demo-verify", "version": "1.0"},
        })
        self.protocol = result["protocolVersion"]
        self.rpc("notifications/initialized", {}, notification=True)
        return result

    def call(self, tool, args=None, allow_error=False):
        result = self.rpc("tools/call", {"name": tool, "arguments": args or {}})
        content = result.get("structuredContent")
        if result.get("isError") and not allow_error:
            raise RuntimeError(content or result)
        if content is None:
            raise RuntimeError(f"{tool} did not return structured content")
        return content


def state_is_terminal(value):
    return value and value.rsplit("_", 1)[-1] in END_STATES


def wait_plan(client, task_id, ticket_id=None):
    deadline = time.monotonic() + 180
    args = {"taskId": task_id}
    if ticket_id is not None:
        args["ticketId"] = ticket_id
    while time.monotonic() < deadline:
        plan = client.call("get_plan", args)
        if state_is_terminal(plan.get("solveState")) and plan["geometryState"] not in {"queued", "running"}:
            if not plan.get("result"):
                raise RuntimeError(plan)
            return plan
        time.sleep(0.75)
    raise TimeoutError(f"Plan {task_id} exceeded 180 seconds")


def assignments(request, result):
    problem = request["problem"]
    return {"routes": [
        {
            "agentId": problem["agents"][route.get("agent", 0)]["id"],
            "ticketIds": [problem["tickets"][i]["id"] for i in route.get("tickets", [])],
        }
        for route in result["solution"].get("routes", [])
    ]}


def without_starting_solution(request):
    return {key: value for key, value in request.items() if key != "initialSolution"}


def bind_scenario_basemap(client, scenario_id, package_id):
    selected = client.api("/v1/basemaps/selection?" + urlencode({"scenario_id": scenario_id, "version": 1}))
    context = selected["context"]
    if selected.get("boundPackageId") != package_id:
        client.api("/v1/basemaps/selection", {
            "context": context, "packageId": package_id,
            "expectedPackageId": selected.get("boundPackageId", ""),
        })


def prepare(client, fixtures, archived_job):
    fixtures.mkdir(parents=True, exist_ok=False)
    info = client.connect()
    recorded = client.api(f"/v1/jobs/{archived_job}")
    version = recorded["scenario"]["version"]
    scene_id = recorded["scenario"]["scenarioId"]
    archived = client.api(f"/v1/scenarios/{scene_id}?version={version}")
    request = without_starting_solution(client.api(f"/v1/jobs/{archived_job}/request"))
    problem = request["problem"]
    if len(problem["tickets"]) != 480 or len(problem["agents"]) != 80 or not problem.get("matrices"):
        raise ValueError("Expected the prepared 480-ticket, 80-unit fixture with a frozen matrix")
    map_selection = client.api("/v1/basemaps/selection?" + urlencode({"job_id": archived_job}))
    package = map_selection.get("package", {}).get("id")
    if not package:
        raise ValueError("Expected the archived plan's offline basemap package")
    # Keep IDs and road matrices stable and reserve one extra job per unit
    # for emergencies only if the historical limits leave no spare slots.
    ticket_caps = [unit.get("maxTickets", 0) for unit in problem["agents"]]
    reserve = all(ticket_caps) and sum(ticket_caps) - max(ticket_caps) < 481
    if reserve:
        for agent in problem["agents"]:
            agent["maxTickets"] += 1
    for scenario_id in (INITIAL, REPLAN):
        for agent in problem["agents"]:
            agent["resourceId"] = f"{scenario_id}/{agent['id']}"
        client.api("/v1/scenarios", {
            "scenarioId": scenario_id,
            "name": "跨区现场服务 · 首次规划" if scenario_id == INITIAL else "跨区现场服务 · 在途重排",
            "request": request,
            "display": archived["scenario"].get("display", {}),
            "origin": archived["scenario"].get("origin", {}),
        })
        bind_scenario_basemap(client, scenario_id, package)
    initial_frozen = client.api(f"/v1/scenarios/{INITIAL}?version=1")["request"]
    write_json(fixtures / "initial.problem.json", without_starting_solution(initial_frozen))
    seed_job = client.call("solve_plan", {"scenarioId": REPLAN, "expectedVersion": 1, "requestId": "seed-baseline"})
    baseline = wait_plan(client, seed_job["taskId"])
    job_id = baseline["jobId"]
    op, change, protected_ticket = replay_plan(client, REPLAN, job_id)
    write_json(fixtures / "baseline.assignments.json", {"routes": [
        {"agentId": route["agentId"], "ticketIds": [stop["ticketId"] for stop in route["stops"]]}
        for route in op["plan"]["routes"]
    ]})
    write_json(fixtures / "execution-snapshot.json", op)
    write_json(fixtures / "change.json", change)
    write_json(fixtures / "fixture.json", {
        "sourceScene": scene_id, "sourceJob": archived_job, "mcpProtocol": info["protocolVersion"],
        "tickets": 480, "agents": 80, "reserveCapacityAdded": reserve, "baselineJobId": job_id,
        "basemapPackageId": package,
        "replayedCompleted": 6, "inServiceTicket": protected_ticket, "change": change,
    })
    print(f"Prepared {fixtures}; baseline job {job_id}; emergency reserve {reserve}", flush=True)


def replay_plan(client, scenario_id, job_id):
    job = client.api(f"/v1/jobs/{job_id}")
    reference = job["scenario"]
    if reference["scenarioId"] != scenario_id:
        raise ValueError("The replay plan must belong to the requested scenario")
    problem = client.api(f"/v1/jobs/{job_id}/request")["problem"]
    op = client.api("/v1/operations/start", {
        "scenario": reference, "at": "28800",
        "actor": "演示调度员", "initialJobId": job_id,
        "policy": {"clock": "EXECUTION_CLOCK_REPLAY", "autoReplan": False, "autoAdopt": False, "options": PROJECT_OPTIONS},
    })
    tickets = {ticket["id"]: ticket for ticket in problem["tickets"]}
    dependent = {problem["tickets"][pair.get("second", 0)]["id"] for pair in problem.get("precedences", [])}

    def replayable(route):
        stops = route.get("stops", [])
        if not stops:
            return False
        ticket = stops[0]["ticketId"]
        return ticket in tickets and ticket not in dependent and tickets[ticket].get("type", 0) in (0, "TICKET_TYPE_SERVICE")

    routes = sorted(
        [route for route in op["plan"]["routes"] if replayable(route)],
        key=lambda route: (int(route["stops"][0]["end"]), route["agentId"]),
    )
    if len(routes) < 8:
        raise ValueError("The published plan needs at least eight active units for a visible replay")
    events = []
    for route in routes[:6]:
        stop = route["stops"][0]
        arrival = max(28860, int(stop["start"]))
        for phase, at in [("EN_ROUTE", 28801), ("ARRIVED", arrival), ("SERVING", arrival + 1), ("COMPLETED", int(stop["end"]) + 1)]:
            events.append((at, route["agentId"], stop["ticketId"], phase))
    for at, agent, ticket, phase in sorted(events):
        op = client.api(f"/v1/operations/{scenario_id}/events", {
            "scenarioId": scenario_id, "expectedRevision": op["revision"],
            "eventId": f"fixture-{ticket}-{phase}", "at": str(at), "actor": "演示调度员",
            "tickets": [{"ticketId": ticket, "agentId": agent, "phase": f"TICKET_PHASE_{phase}"}],
        })
    protected = routes[6]
    protected_ticket = protected["stops"][0]["ticketId"]
    for phase in ("EN_ROUTE", "ARRIVED", "SERVING"):
        op = client.api(f"/v1/operations/{scenario_id}/events", {
            "scenarioId": scenario_id, "expectedRevision": op["revision"],
            "eventId": f"fixture-{protected_ticket}-{phase}",
            "at": str(int(op["at"]) + 1), "actor": "演示调度员",
            "tickets": [{
                "ticketId": protected_ticket, "agentId": protected["agentId"],
                "phase": f"TICKET_PHASE_{phase}",
                **({"remainingServiceSeconds": "300"} if phase == "SERVING" else {}),
            }],
        })
    fault = routes[7]
    template = tickets[fault["stops"][0]["ticketId"]]
    change = {
        "scenarioId": scenario_id, "expectedRevision": int(op["revision"]),
        "disabledAgentId": fault["agentId"],
        "urgentTicket": {
            "ticketId": "URGENT-01", "locationId": problem["locations"][template.get("location", 0)]["id"],
            "durationSeconds": 600, "latestStartSeconds": max(int(op["at"]) + 7200, int(template.get("latestStart", 0))),
            "skillsRequired": template.get("skillsRequired", []),
        },
    }
    return op, change, protected_ticket


def evaluate(cli, request, plan, extra=None):
    work = subprocess.run([str(cli), "eval-plan", str(request), str(plan), *(extra or [])],
                          capture_output=True, encoding="utf-8", check=False,
                          creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
    if work.returncode:
        raise RuntimeError(work.stderr.strip())
    return json.loads(work.stdout)


def verify(client, fixtures, output, cli, *, threads=4, search_ms=30_000, wait_for_result=False, pinned_scene=False):
    output.mkdir(parents=True, exist_ok=False)
    info = client.connect()
    tools = client.rpc("tools/list", {})["tools"]
    assert {tool["name"] for tool in tools} == {"list_scenarios", "solve_plan", "replan", "get_plan", "adopt_plan"}
    assert any(row["scenarioId"] == INITIAL for row in client.call("list_scenarios")["scenarios"])
    try:
        client.http("/mcp", {"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
                    "application/json, text/event-stream", {"Origin": "http://example.org"})
    except HTTPError as error:
        assert error.code == 403, error
    else:
        raise AssertionError("Cross-origin MCP request was not rejected")
    if client.token:
        message = {"jsonrpc": "2.0", "id": 1, "method": "tools/list"}
        headers = {"X-Forwarded-For": "192.0.2.101", "MCP-Protocol-Version": client.protocol}
        try:
            client.http("/mcp", message, "application/json, text/event-stream", {**headers, "Authorization": ""})
        except HTTPError as error:
            assert error.code == 401, error
        else:
            raise AssertionError("Forwarded unauthenticated MCP request was accepted")
        client.http("/mcp", message, "application/json, text/event-stream", headers)
    diagnostics = []
    change = read_json(fixtures / "change.json")
    args = {"scenarioId": INITIAL, "expectedVersion": 1, "requestId": "verify-initial"}
    if pinned_scene:
        rejected = client.call("solve_plan", {"scenarioId": INITIAL, "requestId": "incomplete-scene"}, allow_error=True)
        assert rejected["code"] == "invalid_request", rejected
        args = {"requestId": "verify-initial"}
    replan = {**change, "requestId": "verify-replan"}
    stale = {**replan, "expectedRevision": max(1, change["expectedRevision"] - 1)}
    assert client.call("replan", stale, allow_error=True)["code"] == "conflict"
    wrong = {**replan, "disabledAgentId": "missing-unit", "requestId": "invalid"}
    assert client.call("replan", wrong, allow_error=True)["code"] == "invalid_request"
    before = client.api(f"/v1/operations/{REPLAN}")
    wrong_skill = {
        **replan,
        "requestId": "invalid-skill",
        "urgentTicket": {**replan["urgentTicket"], "skillsRequired": ["installation"]},
    }
    rejected = client.call("replan", wrong_skill, allow_error=True)
    assert rejected["code"] == "invalid_request" and "安装" in rejected["message"], rejected
    assert client.api(f"/v1/operations/{REPLAN}") == before
    assert int(before["revision"]) == change["expectedRevision"]
    for phase, tool, context in [("initial", "solve_plan", args), ("replan", "replan", replan)]:
        inline = wait_for_result or (pinned_scene and phase == "initial")
        if wait_for_result and not (pinned_scene and phase == "initial"):
            context = {**context, "waitForResult": True}
        submitted_ms = time.time_ns() / 1_000_000
        started = time.monotonic()
        job = client.call(tool, context)
        delivered_ms = time.time_ns() / 1_000_000
        tool_wall = time.monotonic() - started
        target = change["urgentTicket"]["ticketId"] if phase == "replan" else None
        if inline:
            assert job.get("result") and state_is_terminal(job.get("solveState")), job
            assert job.get("viewUrl"), job
            assert not target or job.get("ticket", {}).get("ticketId") == target, job
            assert "assignments" not in job, job
        if pinned_scene and phase == "initial":
            assert job["scenarioId"] == INITIAL and job["scenarioVersion"] == 1, job
        retry = client.call(tool, {**context, "waitForResult": False})
        assert retry["taskId"] == job["taskId"]
        plan = wait_plan(client, job["taskId"], target)
        wall = time.monotonic() - started
        job_id = plan["jobId"]
        job_status = client.api(f"/v1/jobs/{job_id}")
        request_url = f"/v1/jobs/{job_id}/request"
        frozen = client.api(request_url)
        result = client.api(f"/v1/jobs/{job_id}/result")
        request_path = output / f"{phase}.request.pb"
        request_path.write_bytes(client.http(request_url, accept="application/protobuf")[0])
        problem_path = output / f"{phase}.problem.json"
        write_json(problem_path, without_starting_solution(frozen))
        result_path = output / f"{phase}.assignments.json"
        ordered = assignments(frozen, result)
        tool_plan = client.call("get_plan", {"taskId": job["taskId"], "includeAssignments": True})
        assert tool_plan["assignments"] == ordered
        write_json(result_path, ordered)
        evaluation = evaluate(cli, request_path, result_path, (
            ["--baseline", str(fixtures / "baseline.assignments.json"), "--urgent-ticket", target]
            if target else []
        ))
        assert plan["threads"] == threads and int(frozen["options"]["timeLimitMs"]) == search_ms
        assert plan["result"]["roadLegs"] > 0, plan
        assert evaluation["qualified"], evaluation
        if target:
            after = client.api(f"/v1/operations/{REPLAN}")
            assert after["plan"]["jobId"] == before["plan"]["jobId"]
            assert after["latestJobId"] == job_id and plan["ticket"]["ticketId"] == target
            assert after["scenario"]["version"] == str(int(before["scenario"]["version"]) + 1)
            assert client.call(tool, {**context, "disabledAgentId": "different-unit"}, allow_error=True)["code"] == "conflict"
        record = {
            "phase": phase, "evidence": "engine-only local MCP preflight",
            "mcpWallSeconds": round(wall, 3), "plan": plan, "independent": evaluation,
            "problemSha256": hashlib.sha256(problem_path.read_bytes()).hexdigest(),
            "firstMcpReplySeconds": round(tool_wall, 3),
            "resultInFirstReply": bool(job.get("result")),
        }
        if inline:
            record["serverTiming"] = {
                "mcpRequestToJobStartMs": round(int(job_status["startedUnixMs"]) - submitted_ms),
                "queueWaitMs": int(job_status["startedUnixMs"]) - int(job_status["createdUnixMs"]),
                "jobStartToFinishMs": int(job_status["finishedUnixMs"]) - int(job_status["startedUnixMs"]),
                "jobFinishToMcpReplyMs": round(delivered_ms - int(job_status["finishedUnixMs"])),
            }
        diagnostics.append(record)
        write_json(output / "local-preflight.json", {"mcpProtocol": info["protocolVersion"], "runs": diagnostics})
        print(f"{phase}: job {job_id}, first MCP reply {tool_wall:.2f}s, {wall:.1f}s MCP + map, {plan['result']['roadLegs']} road legs, qualified={evaluation['qualified']}", flush=True)


@contextmanager
def isolated_server(server, data, log, *, threads=4, search_ms=30_000, search_strategy="baseline", pinned_scene=False):
    with socket.socket() as address:
        address.bind(("127.0.0.1", 0))
        port = address.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    client = Client(base)
    client.token = secrets.token_urlsafe(24)
    with log.open("w", encoding="utf-8") as stream:
        process = subprocess.Popen([
            str(server.resolve()), "--bind", f"127.0.0.1:{port}", "--mcp-origin", base,
            "--data-dir", str(data.resolve()), "--total-threads", str(threads),
            "--mcp-threads", str(threads), "--mcp-search-ms", str(search_ms),
            "--mcp-search-strategy", search_strategy,
            *(["--mcp-default-scenario", f"{INITIAL}@1"] if pinned_scene else []),
            "--geo-prefetch", "false", "--basemap-auto", "false",
        ], stdout=stream, stderr=stream, env={**os.environ, "VRP_API_TOKEN": client.token},
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        try:
            for _ in range(60):
                if process.poll() is not None:
                    raise RuntimeError(f"Server exited; see {log}")
                try:
                    client.api("/healthz")
                    break
                except OSError:
                    time.sleep(0.25)
            else:
                raise TimeoutError(f"Server did not start; see {log}")
            yield client
        finally:
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=15)


def local(args):
    root = args.run_directory.resolve()
    root.mkdir(parents=True, exist_ok=False)
    seed, trial, fixtures, output = (root / part for part in ("seed", "trial", "fixtures", "preflight"))
    clone(args.source_data, seed)
    with isolated_server(args.server, seed, root / "seed.log") as client:
        prepare(client, fixtures, "1")
    clone(seed, trial)
    with isolated_server(
        args.server, trial, root / "trial.log",
        threads=args.mcp_threads, search_ms=args.mcp_search_ms, search_strategy=args.mcp_search_strategy,
        pinned_scene=args.pinned_scene,
    ) as client:
        verify_options(client, fixtures, output, args)
        if args.capture:
            from smoke_workbuddy_panel import capture
            capture(client.base, read_json(output / "local-preflight.json"), output / "workbench", token=client.token)


def verify_options(client, fixtures, output, args):
    verify(
        client, fixtures, output, args.cli.resolve(), threads=args.mcp_threads,
        search_ms=args.mcp_search_ms, wait_for_result=args.wait_for_result, pinned_scene=args.pinned_scene,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    backup = sub.add_parser("copy", help="SQLite-consistent clone of a prepared service data directory")
    backup.add_argument("--source-data", type=Path, required=True)
    backup.add_argument("--data-dir", type=Path, required=True)
    seed = sub.add_parser("prepare", help="Create the two demo scenes, published baseline and replay snapshot")
    seed.add_argument("--base-url", required=True)
    seed.add_argument("--fixtures", type=Path, required=True)
    seed.add_argument("--archived-job", default="1")
    smoke = sub.add_parser("verify", help="Run both real MCP paths on a disposable clone and score the results")
    smoke.add_argument("--base-url", required=True)
    smoke.add_argument("--fixtures", type=Path, required=True)
    smoke.add_argument("--output", type=Path, required=True)
    smoke.add_argument("--cli", type=Path, default=ROOT / "target/release" / f"vrp{BIN_SUFFIX}")
    test = sub.add_parser("local", help="Prepare and test on a disposable pair of local data directories")
    test.add_argument("--source-data", type=Path, required=True)
    test.add_argument("--run-directory", type=Path, required=True)
    test.add_argument("--server", type=Path, default=ROOT / "target/release" / f"vrp-server{BIN_SUFFIX}")
    test.add_argument("--cli", type=Path, default=ROOT / "target/release" / f"vrp{BIN_SUFFIX}")
    test.add_argument("--capture", action="store_true", help="Capture local browser QA frames")
    test.add_argument("--mcp-search-strategy", choices=["baseline", "nearest"], default="baseline")
    for command in (smoke, test):
        command.add_argument("--mcp-threads", type=int, default=4)
        command.add_argument("--mcp-search-ms", type=int, default=30_000)
        command.add_argument("--wait-for-result", action="store_true", help="Require the first MCP reply to include results")
        command.add_argument("--pinned-scene", action="store_true", help="Verify initial planning using only requestId against the pinned fixture")
    args = parser.parse_args()
    if args.action == "copy":
        clone(args.source_data, args.data_dir)
    elif args.action == "prepare":
        prepare(Client(args.base_url), args.fixtures, args.archived_job)
    elif args.action == "verify":
        verify_options(Client(args.base_url), args.fixtures, args.output, args)
    else:
        local(args)


if __name__ == "__main__":
    main()
