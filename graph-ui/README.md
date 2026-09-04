# Beholder graph UI prototype

This Tauri 2 desktop prototype renders a SvelteKit/shadcn-svelte workspace graph with `force-graph`. Its Tauri commands read registered workspaces and typed topology snapshots from the running Beholder daemon.

## Run

Install the pinned tools and Beholder binaries from the repository root, then confirm that the daemon is running:

```sh
mise install
just install
beholder daemon status
```

The UI needs at least one registered workspace. Inspect the current registry and, if needed, register the repositories that belong to one workspace:

```sh
beholder workspace list
beholder workspace register beholder "$PWD"
```

Then launch the desktop app:

```sh
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

The app lists workspaces and loads revision-consistent topology snapshots through `beholder-daemon-client`. Its local context, dependency, impact, and trace controls project the pinned snapshot without issuing additional daemon queries.
