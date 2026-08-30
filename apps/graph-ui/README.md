# Beholder graph UI prototype

This Tauri 2 desktop prototype renders a SvelteKit/shadcn-svelte workspace graph with `force-graph`. Its realistic Fresha-shaped fixture exists only in `src-tauri/src/fixture.rs` and reaches the frontend through the `list_workspaces` and `load_graph` Tauri commands. It does not add or bypass a Beholder daemon API.

## Run

Install the pinned tools from the repository root, then launch the desktop app:

```sh
mise install
cd apps/graph-ui
pnpm install
pnpm tauri dev
```

The prototype opens on the aggregated workspace. Zoom to expand repositories into modules, files, and typed entities; click a repository to narrow to it, or click an entity to re-root around every reachable upstream and downstream entity. **Clear focus** restores the unfocused topology, while the breadcrumb returns to repository and workspace views.

Focused checks:

```sh
pnpm test
pnpm check
pnpm build
cargo check -p beholder-graph-ui
```

## Backend capability required

Replacing the fixture without changing the initial seedless workspace experience needs one bounded, revision-consistent workspace/repository topology query that returns the existing typed entity and semantic-edge DTOs plus explicit truncation metadata. Existing `dependencies`, `impact`, and `context` queries can then serve entity re-rooting and lazy detail; the missing capability is discovery of a safe initial workspace projection. That belongs in a separate backend ticket after this interaction is validated.
