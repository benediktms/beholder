## 1. Baseline and contracts

- [ ] 1.1 Deliver A1's frozen repo-only/current-MCP task corpus, gold evidence, rubric, manifest, cache/model controls, raw transcript schema, and self-check; verify fixed transcripts, unavailable arms, stale/truncated failure scoring, and paired baseline are captured before A3-A5/B product work.
- [ ] 1.2 Finalize A2 intent schemas, defaults/hard maxima, entity resolution, errors, origin/readiness/truncation and compatibility against `query-api`; verify `mise exec -- openspec validate --all --strict`.

## 2. Reusable services and adapters

- [ ] 2.1 Implement A3 bounded overview/find/inspect/multipath-trace/grouped-impact services above immutable snapshots; verify `cargo test -p beholder-domain -p beholder-dto -p beholder-adapters-mnestic --features beholder-adapters-mnestic/sqlite`.
- [ ] 2.2 Add A3 deterministic resolution, bounds, evidence retention, revision-race, and error tests; verify `cargo test -p beholder-protocol -p beholder-daemon-client -p beholder-daemon`.
- [ ] 2.3 Register A4's five additive MCP tools with thin mappings and client shields; verify `cargo test -p beholder-mcp -p beholder-cli -p beholder-presentation` and `bash scripts/test-mcp-integration.sh`.
- [ ] 2.4 Run A4 arms 1-3 against A1's frozen controls; verify the report distinguishes measured results, unavailable arms, and adoption-gate status.

## 3. CLI compatibility

- [ ] 3.1 Implement A5 `query` and `debug inspect` routing with legacy aliases retaining handlers/semantics; verify clap help snapshots and alias/new-path exit-error parity.
- [ ] 3.2 Migrate CLI/docs/scripts references without deleting supported commands; verify `cargo fmt --all -- --check`, affected `cargo clippy -p <affected-package> --all-targets --no-deps -- -D warnings`, and `git diff --check`.
