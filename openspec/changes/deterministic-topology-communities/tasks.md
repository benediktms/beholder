## 1. Pure topology contracts

- [ ] 1.1 Deliver B1 algorithm-neutral projection/partition/status DTOs and canonical JSON identities; verify golden serialization, permutations, hash sensitivity, and malformed/non-finite weight rejection with `cargo test -p beholder-topology-analysis`.
- [ ] 1.2 Deliver B2's complete participant/layer/filter/direction/evidence transformation over every entity and relation kind; verify fixtures for parallel/self edges, hubs, generated/tests/external/unknown, and omission accounting with `cargo test -p beholder-topology-analysis`.

## 2. Empirically gated partitions

- [ ] 2.1 Execute B3 source/license/API audit and fixture/differential/target/repeatability evaluation before dependency adoption; verify the recorded gate yields either a validated native single-thread CPM backend or a do-not-adopt report without a production dependency.
- [ ] 2.2 Deliver B4 independent coarse/standard/fine profiles and diagnostics without hierarchy; verify non-nested overlap, stable membership IDs, and absent-resolution behavior with `cargo test -p beholder-topology-analysis`.

## 3. Derived lifecycle

- [ ] 3.1 Deliver B5a separate SQLite artifacts/status/selected pointer/retention; verify exact-key selection, failure visibility, and eviction without canonical mutation using `cargo test -p beholder-derived-state`.
- [ ] 3.2 Deliver B5b explicit coalesced five-attempt topology jobs with recovery/cancel/currentness fences; verify restart, duplicate enqueue, R/R+1 supersession, terminal failure, and canonical concurrency with `cargo test -p beholder-protocol -p beholder-daemon-client -p beholder-daemon`.
- [ ] 3.3 Run affected formatter/linter checks; verify `cargo fmt --all -- --check`, affected `cargo clippy -p <affected-package> --all-targets --no-deps -- -D warnings`, and `git diff --check`.
