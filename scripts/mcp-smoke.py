#!/usr/bin/env python3
import argparse
import copy
import json
import select
import subprocess
import sys
import time


parser = argparse.ArgumentParser()
parser.add_argument("--max-hops", type=int, default=1)
parser.add_argument("--repository")
parser.add_argument("--evidence-target")
parser.add_argument("--legacy-target")
parser.add_argument("binary")
parser.add_argument("workspace")
parser.add_argument("query")
parser.add_argument("expected_entity")
parser.add_argument("expected_nodes", nargs="*")
arguments = parser.parse_args()
binary = arguments.binary
workspace = arguments.workspace
query = arguments.query
expected_entity = arguments.expected_entity
expected_nodes = arguments.expected_nodes
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
    result.get("analysis", {}).pop("diagnostics", None)
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
    freshness = result.get("freshness")
    if isinstance(freshness, dict):
        freshness = dict(freshness)
        freshness.pop("indexing", None)
        result["freshness"] = freshness
    if "truncation_reasons" in result:
        result["truncation_reasons"] = sorted(result["truncation_reasons"])
    result.get("analysis", {}).pop("diagnostics", None)
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
    structured = result["structuredContent"]
    content = result.get("content", [])
    if len(content) != 1 or content[0].get("type") != "text":
        raise AssertionError(f"tool result omitted raw JSON content: {result}")
    if json.loads(content[0]["text"]) != structured:
        raise AssertionError("raw and structured tool results disagree")
    return structured


def assert_structured_evidence(result, target):
    edges = [
        edge
        for edge in result["edges"]
        if edge["kind"] == "calls" and edge["to"] == target
    ]
    if len(edges) != 1:
        raise AssertionError(f"expected one calls edge to {target!r}: {edges}")
    evidence = edges[0]["evidence"]
    if len(evidence) != 2:
        raise AssertionError(f"parallel call evidence collapsed: {evidence}")
    if any(item["range"] is None for item in evidence):
        raise AssertionError(f"structured call range was not preserved: {evidence}")
    if len({json.dumps(item["range"], sort_keys=True) for item in evidence}) != 2:
        raise AssertionError(f"parallel call ranges were not distinct: {evidence}")
    if len({json.dumps(item["contexts"], sort_keys=True) for item in evidence}) != 2:
        raise AssertionError(f"parallel call contexts were not distinct: {evidence}")

    nested = False
    for item in evidence:
        contexts = item["contexts"]
        if (
            not contexts
            or contexts[0].get("kind") != "callable_clause"
            or contexts[0].get("role") != "enclosing"
        ):
            raise AssertionError(f"enclosing callable context was not first: {contexts}")
        lexical_end = len(contexts)
        if (
            contexts[-1].get("kind") == "callable_clause"
            and contexts[-1].get("role") == "selected_target"
        ):
            lexical_end -= 1
        lexical = contexts[1:lexical_end]
        if any(
            context["kind"] not in {"condition_arm", "pattern_arm"}
            for context in lexical
        ):
            raise AssertionError(
                f"lexical contexts were not ordered between callable contexts: {contexts}"
            )
        ranges = [context["arm_range"] for context in lexical] + [item["range"]]
        for outer, inner in zip(ranges, ranges[1:]):
            outer_start = (outer["start"]["line"], outer["start"]["character"])
            outer_end = (outer["end"]["line"], outer["end"]["character"])
            inner_start = (inner["start"]["line"], inner["start"]["character"])
            inner_end = (inner["end"]["line"], inner["end"]["character"])
            if not outer_start <= inner_start <= inner_end <= outer_end:
                raise AssertionError(f"lexical contexts were not outer-first: {contexts}")
        nested |= len(lexical) >= 2
    if not nested:
        raise AssertionError(f"no call retained nested lexical contexts: {evidence}")


def assert_legacy_evidence(result, target):
    evidence = [
        item
        for edge in result["edges"]
        if edge["to"] == target
        for item in edge["evidence"]
    ]
    legacy = [item for item in evidence if item["range"] is None and item["contexts"] == []]
    if len(legacy) != 1:
        raise AssertionError(
            f"expected one unmodified legacy evidence record for {target!r}: {evidence}"
        )
    required = {"source", "repository", "path", "line", "range", "detail", "contexts"}
    if not required <= legacy[0].keys():
        raise AssertionError(f"legacy evidence fields were not projected: {legacy[0]}")
    if not legacy[0]["path"] or legacy[0]["line"] is None:
        raise AssertionError(f"legacy location fields were not projected: {legacy[0]}")


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
    detailed_search = canonical_search_result(call(
        "search_entities", {**search_input, "include_diagnostics": True}
    ))
    if first_search != detailed_search:
        raise AssertionError("summary and detailed search disagree without diagnostics")
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
        "max_hops": arguments.max_hops,
    }
    first_traversal = canonical_traversal_result(call("traverse_graph", traversal_input))
    detailed_traversal = canonical_traversal_result(call(
        "traverse_graph", {**traversal_input, "include_diagnostics": True}
    ))
    if first_traversal != detailed_traversal:
        raise AssertionError("summary and detailed traversal disagree without diagnostics")
    node_ids = {node["id"] for node in first_traversal["nodes"]}
    for expected in expected_nodes:
        if expected not in node_ids:
            raise AssertionError(f"traversal did not contain {expected!r}")
    repository = arguments.repository or entity["id"].removeprefix("repo://").split(
        "/rust/", 1
    )[0]
    filtered = call("traverse_graph", {
        **traversal_input,
        "target_repositories": [repository],
    })
    if filtered["query"]["target_repositories"] != [repository]:
        raise AssertionError("repository target was not preserved")

    started = time.perf_counter()
    second_search = canonical_search_result(call("search_entities", search_input))
    search_ms = (time.perf_counter() - started) * 1000
    started = time.perf_counter()
    second_traversal = canonical_traversal_result(call("traverse_graph", traversal_input))
    traversal_ms = (time.perf_counter() - started) * 1000
    if first_search != second_search or first_traversal != second_traversal:
        raise AssertionError("daemon ordering or metadata changed between warm calls")
    if arguments.evidence_target:
        assert_structured_evidence(second_traversal, arguments.evidence_target)
    if arguments.legacy_target:
        assert_legacy_evidence(second_traversal, arguments.legacy_target)
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
