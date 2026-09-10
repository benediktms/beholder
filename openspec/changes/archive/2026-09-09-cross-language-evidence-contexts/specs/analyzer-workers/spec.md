## ADDED Requirements

### Requirement: Context-aware worker evidence

Analyzer worker observations and overrides SHALL carry the same typed source range
and ordered context contract as baseline observations. Conversion at the daemon
boundary SHALL canonicalize valid typed worker evidence before publication.

#### Scenario: Worker contribution round-trip

- **WHEN** a worker returns an observation with structured evidence context
- **THEN** publication and later semantic queries preserve its range, excerpts, context order, confidence, and provenance

#### Scenario: Invalid worker context

- **WHEN** a worker returns an unknown required context kind or an invalid source range
- **THEN** protocol conversion rejects the contribution rather than publishing ambiguous evidence

### Requirement: Exact compiler callable selection

Compiler workers SHALL attach selected-target callable context only when compiler
output identifies one declaration and its exact source range. Compiler events
SHALL remain correlated to individual source coordinates rather than being
collapsed solely by semantic target.

#### Scenario: Compiler reports only a shared callable identity

- **WHEN** an Elixir worker reports only module, function, and arity or another worker reports multiple possible overload declarations
- **THEN** the worker emits no selected-target callable context

#### Scenario: Compiler reports one declaration range

- **WHEN** a compiler event identifies one exact declaration range for a call occurrence
- **THEN** the worker attaches that declaration as the selected-target context for that occurrence only

#### Scenario: Macro or synthesized compiler event

- **WHEN** a compiler event has no exact user-source call coordinate
- **THEN** it does not inherit context from a nearby source call
