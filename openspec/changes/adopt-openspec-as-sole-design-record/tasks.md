## 1. Establish OpenSpec-only governance

- [ ] 1.1 Update `openspec/config.yaml` so active change designs own architectural rationale, archived changes own completed decision history, and new ADRs are not part of the workflow; verify the configured artifact rules still parse with `openspec validate --all --strict`.
- [ ] 1.2 Replace the transitional ADR migration crosswalk in `openspec/README.md` with the permanent ownership and archive-discovery workflow; verify every documented command and repository path exists or is provided by the pinned OpenSpec CLI.

## 2. Make current specs self-contained

- [ ] 2.1 Rewrite only the `Purpose` text that cites `docs/adr/` in `analyzer-workers`, `desktop-graph`, `indexing-pipeline`, `observability`, `runtime-plugins`, and `workspace-state`; verify a focused diff shows no changes beneath each spec's `## Requirements` heading.
- [ ] 2.2 Search every main spec for ADR paths or labels and remove any remaining dependency on the deleted decision system; verify `rg -n 'docs/adr|ADR [0-9]{4}|ADRs?' openspec/specs` returns no matches.

## 3. Update supporting documentation

- [ ] 3.1 Update `README.md` to identify OpenSpec specs and archived changes as the contract and decision sources, remove ADR links, and describe the implemented Rust, Elixir, TypeScript, and runtime-plugin surfaces accurately; verify every referenced local path exists.
- [ ] 3.2 Replace remaining ADR references in `docs/` with the relevant current capability spec or focused evidence document without turning roadmap or benchmark prose into requirements; verify `rg -n 'docs/adr|ADR [0-9]{4}|ADRs?' docs --glob '!adr/**'` returns no matches.
- [ ] 3.3 Delete all eight files under `docs/adr/` and remove the empty directory; verify `test ! -d docs/adr` succeeds and Git reports exactly the expected ADR deletions.

## 4. Enforce the specification workflow

- [ ] 4.1 Add `mise exec -- openspec validate --all --strict` to the existing CI `checks` job on Linux only, before broader Moon checks; verify the workflow contains one strict OpenSpec invocation and that its condition excludes macOS.

## 5. Validate the migration

- [ ] 5.1 Search the repository, excluding this historical change record and Git metadata, for live ADR paths, labels, or authoring instructions and remove any residual references; verify the focused `rg` search returns no matches.
- [ ] 5.2 Run the pinned `openspec validate --all --strict` and verify all current specs plus this active documentation-only change pass strict validation.
- [ ] 5.3 Review the final diff and verify it changes only documentation, OpenSpec configuration and artifacts, and CI configuration—no runtime source, protocol, generated binding, dependency, or behavioral requirement content.
