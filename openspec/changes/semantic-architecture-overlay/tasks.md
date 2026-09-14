## 1. Gate and deterministic inputs

- [ ] 1.1 Confirm B's preregistered useful-B gate before C implementation; verify the decision and A1 checkpoint evidence are recorded, otherwise stop C with no provider changes.
- [ ] 1.2 Deliver C1 minimal evidence-backed units independent of community IDs; verify reorder, missing-community, and multi-unit-community cases with `cargo test -p beholder-architecture-query`.
- [ ] 1.3 Deliver C2 bounded deterministic evidence packs/context hashes; verify ordering, bounds, multibyte excerpts, and source-exposure limits with `cargo test -p beholder-architecture-query`.

## 2. Optional interpretation

- [ ] 2.1 Deliver C3 optional isolated provider protocol/worker and complete cache identity; verify no-provider, timeout/crash/cancel, invalid output, and cache-hit paths with `cargo test -p beholder-architecture-query`.
- [ ] 2.2 Deliver C4 whitelist reducer with traceable accepted/rejected suggestions; verify conflicts, cycles, unknown members, size limits, determinism, and rollback with `cargo test -p beholder-architecture-query`.
- [ ] 2.3 Deliver C5 retained-artifact continuity with uncertainty; verify rename/split/merge/disappearance/equal-overlap/missing-predecessor cases with `cargo test -p beholder-architecture-query`.
- [ ] 2.4 Verify focused protocol/daemon effects and formatting with `cargo test -p beholder-protocol -p beholder-daemon-client -p beholder-daemon`, `cargo fmt --all -- --check`, and affected clippy.
