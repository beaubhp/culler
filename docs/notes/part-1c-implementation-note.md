# Part 1C Implementation Note

Part 1C implements conservative intraprocedural lookup-state analysis for references already
resolved by Part 1B. It enriches `ReferenceFact.binding_state` and does not alter lexical targets,
resolve imports across modules, model annotations, or emit findings.

Implemented semantics:

- `ReferenceBindingState::{Analyzed, NotAnalyzed}` replacing the Part 1B `NotApplicable`
  placeholder for debug references
- orthogonal `BindingState` with local may-execution, exact binding candidates, residual lookup,
  and flow uncertainty
- deterministic binding-set and flow-uncertainty arenas in `debug references`
- execution-context flow status with explicit unsupported contexts
- exact binding sets with no candidate truncation
- straight-line strong updates for assignments, imports, definitions, parameters, and `del`
- function and class definition binding order: definition-time expressions before final binding,
  and class suites executed immediately
- branch joins for `if`, conditional expressions, boolean short-circuiting, and named expressions
- loop fixed points for `for` and `while`, including zero-iteration paths, `break`, `continue`,
  and `else`
- same-invocation `global` and `nonlocal` writes through their resolved symbol slots
- local unreachable references after local termination
- class-local then global then builtin fallback applied during lookup evaluation
- eager list/set/dict comprehension body analysis with isolated targets
- generator-expression body analysis as deferred runtime-sensitive execution
- conservative call, `await`, and `yield` barriers for global and closure-cell assumptions
- conservative `try`/`except`/`else`/`finally`, exception-target cleanup, and complex-exception
  uncertainty
- `match` capture binding flow and failed-partial-match uncertainty

Deliberately conservative:

- ordinary calls are treated as opaque for module-global and closure-cell state
- complex exceptional paths preserve concrete candidates where known and add uncertainty rather than
  manufacturing precision
- contexts that exceed fixed-point resource budgets are marked unsupported and affected references
  become `NotAnalyzed`
- annotation and type-parameter scope semantics remain deferred
- no public dead-code candidates or findings are emitted

Part 1C fixtures cover straight-line redefinition, partial branches, deletion, unreachable local
blocks, same-invocation global writes, opaque call residuals, exception target cleanup, match
captures, class fallback, eager comprehensions, generator expressions, deterministic JSON
snapshots, exact binding-set arenas, uncertainty arenas, and context flow statuses.

Verification:

```bash
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo check --manifest-path fuzz/Cargo.toml
git diff --check
```
