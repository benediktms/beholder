# Beholder graph UI prototype

This Tauri 2 desktop prototype renders a SvelteKit/shadcn-svelte workspace graph with `force-graph`. Its realistic fixture exists only in `../crates/graph-ui/src/fixture.rs` and reaches the frontend through the `list_workspaces` and `load_graph` Tauri commands. It does not add or bypass a Beholder daemon API.

## Run

Install the pinned tools from the repository root, then launch the desktop app:

```sh
mise install
just graph-ui
```

The prototype opens on the workspace's typed entities. Use the repository and semantic filters to narrow the topology, or click an entity to persistently highlight upstream neighbours in teal, downstream neighbours in orange, and directional relationships without changing the graph layout. Node size reflects visible incident edges. **Clear focus** restores the unfocused topology.

Focused checks:

```sh
pnpm --filter @beholder/graph-ui test
pnpm --filter @beholder/graph-ui check
pnpm --filter @beholder/graph-ui build
cargo check -p beholder-graph-ui
```

## Backend capability required

Replacing the fixture without changing the initial seedless workspace experience needs one bounded, revision-consistent workspace/repository topology query that returns the existing typed entity and semantic-edge DTOs plus explicit truncation metadata. Existing `dependencies`, `impact`, and `context` queries can then serve optional deeper traversal and lazy detail; the missing capability is discovery of a safe initial workspace projection. That belongs in a separate backend ticket after this interaction is validated.
