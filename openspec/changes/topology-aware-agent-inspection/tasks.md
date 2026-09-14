## 1. Exact-revision topology queries

- [ ] 1.1 Deliver D1 topology-aware inspect after A4 and B3/B4/B5b; verify ready/absent/failed/wrong-revision/support-only/bounded cases with `cargo test -p beholder-architecture-query -p beholder-mcp -p beholder-cli`.
- [ ] 1.2 Deliver D2 bounded progressive `architecture_map`; verify all semantic/Leiden/raw/required fallback branches, every bound, omissions, exact revision, and no-provider spy with `cargo test -p beholder-architecture-query -p beholder-mcp -p beholder-cli`.

## 2. Evidence-preserving compression and evaluation

- [ ] 2.1 Deliver D3 compression only over bounded canonical trace/impact; verify branches/convergence, mixed membership, unavailable semantic data, witnesses, and truncation propagation with `cargo test -p beholder-architecture-query`.
- [ ] 2.2 Run D4's five-arm repeated blind evaluation from the frozen A1 manifest; verify unavailable arms, scorer agreement/redaction audit, correctness/misleading gates, and explicit adopt/revise/stop report.
- [ ] 2.3 Verify MCP integration and focused hygiene with `bash scripts/test-mcp-integration.sh`, `cargo fmt --all -- --check`, affected clippy, and `git diff --check`.
