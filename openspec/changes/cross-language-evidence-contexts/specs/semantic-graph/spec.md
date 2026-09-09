## ADDED Requirements

### Requirement: Structured source evidence

Each evidence record SHALL preserve its source kind, repository, path, existing
one-based line, optional source range, detail, and ordered syntactic contexts.
Source positions SHALL use zero-based UTF-16 line and character offsets with an
end-exclusive end position. Source excerpts SHALL retain exact source text and
their corresponding range.

#### Scenario: Same-line observations remain distinct

- **WHEN** two observations for the same semantic edge occur on the same source line at different ranges
- **THEN** aggregation retains both evidence records with their distinct ranges and contexts

#### Scenario: Nested context ordering

- **WHEN** an observation is nested inside more than one syntactic selection
- **THEN** its contexts are ordered from the outermost lexical context to the innermost
- **AND** an enclosing callable clause precedes lexical selection contexts
- **AND** an exact selected callable target follows lexical selection contexts

### Requirement: Syntactic control-selection context

Call observations inside selected syntax SHALL identify the arm containing the
call without claiming which arm executed at runtime. Conditional context SHALL
represent `if`, `cond`, ternary, and template-if constructs and their syntactic
arms. Pattern context SHALL represent match, case, switch-statement, and
switch-expression selectors, patterns, optional guards, case/default status, and
arm ranges.

#### Scenario: Calls in selecting expressions

- **WHEN** a call evaluates a condition, selector, pattern, or guard
- **THEN** that call does not receive the arm selected by the expression it helps evaluate
- **AND** any enclosing outer contexts remain present

#### Scenario: Rust conditional and match arms

- **WHEN** Rust calls occur in `if`, `else if`, `else`, or `match` bodies
- **THEN** each call carries its containing condition or pattern arm with exact source excerpts and ranges
- **AND** an `else` records only its immediately governing condition rather than inferred false conditions from the full chain
- **AND** a wildcard match pattern remains a case rather than becoming a default arm

#### Scenario: Elixir case and cond clauses

- **WHEN** Elixir calls occur in `case` or `cond` clause bodies
- **THEN** each call carries its containing pattern or condition clause
- **AND** selector, condition, pattern, and guard calls do not receive that clause context

#### Scenario: JavaScript and TypeScript selection arms

- **WHEN** JavaScript or TypeScript calls occur in ternary consequences or alternatives, or in switch case bodies
- **THEN** each call carries its containing syntactic arm
- **AND** fall-through statements belong to the case whose body contains them

#### Scenario: Svelte template selections

- **WHEN** a call occurs in a Svelte template `{#if}`, `{:else if}`, `{:else}`, or ternary expression
- **THEN** the component module owns the call observation and its containing template context
- **AND** a Svelte `else if` is represented as one syntactic arm

#### Scenario: C# switch selections

- **WHEN** C# calls occur in switch-statement or switch-expression arms
- **THEN** each call carries its containing pattern and optional `when` guard
- **AND** a discard pattern remains a case while only explicit default syntax is marked default

### Requirement: Callable-clause context

Evidence SHALL distinguish declaration, enclosing-clause, and selected-target
callable contexts. A callable clause SHALL include its complete source head or
signature, optional guard, and definition range. Selected-target context SHALL be
emitted only for an exact, uniquely identified declaration range.

#### Scenario: Elixir function clauses

- **WHEN** multiple Elixir clauses share a name and arity
- **THEN** each definition observation retains evidence for its own complete head and optional guard while the clauses keep one semantic function identity
- **AND** every call occurrence retains its own enclosing clause context

#### Scenario: Exact callable selection

- **WHEN** a compiler uniquely resolves an Elixir clause or overloaded Rust, TypeScript, JavaScript, or C# declaration to an exact source range
- **THEN** the call evidence includes that declaration as a selected-target context

#### Scenario: Ambiguous callable selection

- **WHEN** resolution supplies only a shared name and arity, uses a heuristic, or leaves multiple declaration ranges possible
- **THEN** no selected-target callable context is emitted
