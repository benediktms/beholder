# Beholder graph UI prototype

This Tauri 2 desktop prototype renders a SvelteKit/shadcn-svelte workspace graph with Sigma/WebGL. Its Tauri commands read registered workspaces and revision-consistent graph views from the running Beholder daemon.

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

The prototype opens on repository communities rather than materializing the complete entity graph. Select a community to stream a bounded neighbourhood into the existing view; endpoints outside the expanded community stay collapsed. Selecting an entity loads its immediate neighbourhood and persistently highlights upstream neighbours in teal, downstream neighbours in orange, and the traversal path in purple. Overview and completed neighbourhood responses are cached by workspace revision.

Focused checks:

```sh
pnpm --filter @beholder/graph-ui test
pnpm --filter @beholder/graph-ui check
pnpm --filter @beholder/graph-ui build
cargo check -p beholder-graph-ui
```

The app lists workspaces, loads an aggregate overview, and consumes streamed neighbourhood batches through `beholder-daemon-client`. Full topology reads remain available as a compatibility contract but are not used when opening the graph UI.
