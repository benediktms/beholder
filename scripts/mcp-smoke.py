#!/usr/bin/env python3
import json
import copy
import select
import subprocess
import sys
import time


binary, workspace, query, expected_entity, *expected_nodes = sys.argv[1:]
process = subprocess.Popen(
    [binary],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
)
request_id = 0


def canonical_search_result(result):
    result = copy.deepcopy(result)
    if "matches" in result:
        result["matches"] = sorted(result["matches"], key=lambda item: item["id"])
    freshness = result.get("freshness")
    if isinstance(freshness, dict):
        freshness = dict(freshness)
        freshness.pop("indexing", None)
        result["freshness"] = freshness
    if "diagnostics" in result:
        result["diagnostics"] = sorted(result["diagnostics"], key=lambda item: (
            item.get("code", ""),
            item.get("severity", 0),
            item.get("detail", ""),
        ))
    return result


def canonical_traversal_result(result):
    result = copy.deepcopy(result)
    if "nodes" in result:
        result["nodes"] = sorted(result["nodes"], key=lambda item: item["id"])
    if "edges" in result:
        result["edges"] = sorted(result["edges"], key=lambda item: (item["from"], item["to"], item["kind"]))
    if "paths" in result:
        result["paths"] = sorted(
            result["paths"],
            key=lambda path: (tuple(path["nodes"]), tuple(path["edges"]), path["termination"]),
        )
    metadata = copy.deepcopy(result.get("metadata", {}))
    if isinstance(metadata, dict):
        metadata.pop("indexing", None)
        result["metadata"] = metadata
    if "truncation_reasons" in result:
        result["truncation_reasons"] = sorted(result["truncation_reasons"])
    return result


def send(method, params=None):
    global request_id
    request_id += 1
    message = {"jsonrpc": "2.0", "id": request_id, "method": method}
    if params is not None:
        message["params"] = params
    process.stdin.write(json.dumps(message) + "\n")
    process.stdin.flush()
    while True:
        ready, _, _ = select.select([process.stdout], [], [], 120)
        if not ready:
            raise TimeoutError(f"timed out waiting for {method}")
        response = json.loads(process.stdout.readline())
        if response.get("id") == request_id:
            if "error" in response:
                raise RuntimeError(response["error"])
            return response["result"]


def notify(method):
    process.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method}) + "\n")
    process.stdin.flush()


def call(name, arguments):
    result = send("tools/call", {"name": name, "arguments": arguments})
    if result.get("isError"):
        raise RuntimeError(result.get("structuredContent", result.get("content")))
    return result["structuredContent"]


try:
    send(
        "initialize",
        {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "beholder-mcp-smoke", "version": "1"},
        },
    )
    notify("notifications/initialized")
    tools = send("tools/list")["tools"]
    if sorted(tool["name"] for tool in tools) != [
        "list_workspaces",
        "search_entities",
        "traverse_graph",
    ]:
        raise AssertionError(f"unexpected tools: {tools}")
    listed = call("list_workspaces", {})
    if workspace not in [item["name"] for item in listed["workspaces"]]:
        raise AssertionError(f"workspace {workspace!r} is not registered")

    search_input = {"workspace": workspace, "query": query}
    first_search = canonical_search_result(call("search_entities", search_input))
    if expected_entity == "-":
        entity = next(iter(first_search["matches"]), None)
    else:
        entity = next(
            (match for match in first_search["matches"] if match["id"] == expected_entity),
            None,
        )
    if entity is None:
        raise AssertionError(f"search did not discover {expected_entity!r}")
    traversal_input = {
        "workspace": workspace,
        "start": entity["id"],
        "direction": "dependencies",
        "max_hops": 1,
    }
    first_traversal = canonical_traversal_result(call("traverse_graph", traversal_input))
    node_ids = {node["id"] for node in first_traversal["nodes"]}
    for expected in expected_nodes:
        if expected not in node_ids:
            raise AssertionError(f"traversal did not contain {expected!r}")

    started = time.perf_counter()
    second_search = canonical_search_result(call("search_entities", search_input))
    search_ms = (time.perf_counter() - started) * 1000
    started = time.perf_counter()
    second_traversal = canonical_traversal_result(call("traverse_graph", traversal_input))
    traversal_ms = (time.perf_counter() - started) * 1000
    if first_search != second_search or first_traversal != second_traversal:
        raise AssertionError("daemon ordering or metadata changed between warm calls")
    failed = send(
        "tools/call",
        {
            "name": "search_entities",
            "arguments": {"workspace": "missing-mcp-smoke-workspace", "query": query},
        },
    )
    error = failed.get("structuredContent", {}).get("error", {})
    if not failed.get("isError") or sorted(error) != ["code", "kind", "message"]:
        raise AssertionError(f"daemon error was not preserved structurally: {failed}")
    for field in ["schema", "revision", "view", "freshness", "query", "matches"]:
        if field not in second_search:
            raise AssertionError(f"search result omitted {field}")
    for field in [
        "schema",
        "revision",
        "view",
        "freshness",
        "query",
        "nodes",
        "edges",
        "paths",
        "traversal",
    ]:
        if field not in second_traversal:
            raise AssertionError(f"traversal result omitted {field}")
    print(
        json.dumps(
            {
                "workspace": workspace,
                "entity": entity["id"],
                "search_warm_ms": round(search_ms, 1),
                "traverse_warm_ms": round(traversal_ms, 1),
            },
            separators=(",", ":"),
        )
    )
finally:
    if process.stdin:
        process.stdin.close()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.terminate()
        process.wait(timeout=5)
    if process.returncode:
        sys.stderr.write(process.stderr.read())
