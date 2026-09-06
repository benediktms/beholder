## Why

Beholder already treats `openspec/specs/` as the normative description of current
behavior, but it still maintains eight ADRs as a parallel architecture record and
links those ADRs throughout its specs and documentation. Several ADR statuses are
now stale, the archive contains no decision history, and CI does not enforce the
OpenSpec contracts, leaving contributors with two overlapping systems and no
single workflow for future decisions.

## What Changes

- Make OpenSpec the sole repository workflow for behavioral specifications and
  architecture decisions: current contracts live in main specs, proposed decisions
  live in change artifacts, and completed rationale lives in archived changes.
- Preserve the non-obvious rationale, rejected alternatives, constraints, and
  measurements from all eight ADRs in this change's design before removing
  `docs/adr/`.
- Remove ADR links from main specs and supporting documents, and update the README
  and OpenSpec guidance to describe the permanent ownership model rather than the
  transitional crosswalk.
- Correct documentation that still describes implemented Elixir, TypeScript, and
  runtime-plugin work as proposed.
- Add strict OpenSpec validation to CI so malformed current specs or change
  artifacts cannot merge unnoticed.
- Keep `docs/VISION.md`, benchmarks, performance evidence, and operational guides
  as non-normative supporting material.

Explicit non-goals:

- No product behavior, public API, protocol, storage, or implementation change.
- No rewrite of existing current requirements merely to remove source citations.
- No conversion of general guides, benchmarks, or the product vision into
  behavioral specifications.

## Capabilities

### New Capabilities

None. This is a documentation-governance and tooling change.

### Modified Capabilities

None. The requirements in all ten current capability specs remain unchanged, so
this change opts out of delta specs with `skip_specs: true`.

## Impact

- Documentation: `README.md`, `docs/adr/`, ADR references in `docs/`,
  `openspec/README.md`, `openspec/config.yaml`, and the introductory purpose text
  of affected main specs.
- Workflow: repository-local OpenSpec authoring becomes the only supported way to
  propose, explain, implement, synchronize, and archive architectural changes.
- CI: one platform runs `openspec validate --all --strict` using the version pinned
  in `mise.toml`.
- Runtime code, schemas, generated bindings, dependencies, and externally visible
  behavior are unaffected.
