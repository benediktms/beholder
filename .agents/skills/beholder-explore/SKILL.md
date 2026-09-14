---
name: beholder-explore
description: Explore indexed codebases with Beholder to explain implementations, trace dependencies across repositories, and assess callers or change impact. Use for semantic code questions when Beholder is available, or when explicitly requested. Does not manage indexing or workspace setup.
---

# Beholder exploration

Use Beholder's connected MCP tools to narrow an investigation, then read the
returned source evidence to explain actual behavior. The graph supplies
relationships; it does not replace reading the implementation.

## Select the workspace and entities

Call `list_workspaces` and match repository identities to the user's question and
current checkout. Honor an explicitly selected workspace. If multiple workspaces
fit and the intended view is unclear, ask rather than choosing an arbitrary one.
Do not register or replace a workspace to make a query work.

Use `search_entities` with the selected `workspace` and an entity name or canonical
ID as `query`. Search is case-sensitive and matches exact names/IDs or prefixes,
not arbitrary substrings or natural-language descriptions. Translate a feature
question into likely code names; use repository and source context to distinguish
matches. For a runtime call-flow question, select the relevant callable rather
than assuming a namespace's edges describe its functions. Reuse IDs returned by
Beholder rather than inventing them.

Start with the default result limit. If a search misses, try a better-supported
name or prefix, or inspect narrowly relevant source to identify one. Missing
matches do not prove that an implementation is absent. If a needed entity cannot
be resolved, explain that boundary and continue with the available evidence.

## Choose the graph question

Call `traverse_graph` with `workspace`, canonical `start`, and the appropriate
`direction`:

- `dependencies`: outgoing relationships, for what an entity uses.
- `dependents`: incoming relationships, for callers and potential change impact.
- Add a canonical `destination` to investigate paths to a particular entity.
- Use `target_repositories` only when each returned path must visit **every**
  listed repository. These are repository identities, not workspace names. For
  independent questions about several repositories, query them separately.

Do not combine `destination` with `target_repositories`. Start with the tool's
default bounds; narrow the question or increase bounds only when a reported limit
prevents answering it. Read `traversal.truncated` and `truncation_reasons` before
treating returned paths as exhaustive.

Open the relevant source locations from the evidence. Verify the behavior that
supports the answer, following unresolved boundaries only as needed. An edge or
path supports a relationship, not a claim that a runtime branch always executes.

## CLI supplements and fallback

Use the installed CLI when a query-specific explanation helps, or when connected
MCP tools are unavailable. For fallback, check `beholder daemon status` and
`beholder workspace list`; use `beholder --help` to confirm available commands.
Do not start or restart services as part of exploration.

With known canonical IDs, useful queries are:

```sh
beholder context --workspace <workspace> <entity>
beholder dependencies --workspace <workspace> <entity>
beholder impact --workspace <workspace> <entity>
beholder trace --workspace <workspace> <from> <to>
beholder why --workspace <workspace> <from> <to>
```

Start with compact output. Use `--json-pretty` for complete typed metadata, `--raw`
for supporting graph evidence, and `--include-tests` when test callers matter.
If neither interface can answer, report the limitation and use scoped source
inspection where possible. Do not invent a CLI entity-search command or imply a
source-derived answer was verified by Beholder.

## Explain the result and its limits

Lead with the answer and cite the source evidence that supports it. Distinguish
graph-backed relationships, source-confirmed behavior, and remaining uncertainty.
Preserve material confidence and generated or inferred provenance when explaining
a path.
Report material freshness, completeness, diagnostics, revision changes between
queries, and traversal limits. Do not silently combine different revisions as one
coherent snapshot, or interpret no returned path as proof of no dependency.

Request detailed diagnostics only when their summary affects the investigation.
Stale or partial results can still be useful when their limits are explicit.
Registration, reindexing, enrichment, and daemon lifecycle changes are separate
tasks and require authorization beyond an exploration request.
