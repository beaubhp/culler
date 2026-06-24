# Part 1B Implementation Note

Part 1B implements lexical reference resolution only. It adds versioned `debug references` output
over the same semantic graph used by `debug bindings`; it does not emit findings or compute
reaching binding sets.

Implemented semantics:

- canonical `ReferenceFact` records with source spelling, mangled semantic name, lexical target,
  lookup semantics, role, phase, origin domain, span, and `binding_state: NotApplicable`
- `GlobalThenBuiltin` lookup through a real module `SymbolId`, even when that symbol has no binding
- `ClassLocalThenGlobalThenBuiltin` lookup for class-body names that are class-local or otherwise
  searched through the class namespace
- whole-block declaration collection for ordinary binding forms, `global`, and `nonlocal`
- semantic diagnostics for invalid `global` and `nonlocal` declarations
- valid global and nonlocal binding targets written to the correct lexical symbol slot
- private-name mangling before symbol interning and resolution while preserving source spelling
- semantic lambda and comprehension scopes, including leftmost-comprehension-iterable context
  ownership
- execution-context ownership based on where expressions execute, not syntactic containment
- f-string and t-string interpolation traversal for supported bare-name loads

Deliberately deferred:

- reaching `BindingId` sets
- branch joins, loops, fixed points, and widening
- maybe-unbound conclusions
- deletion flow effects
- annotation and type-parameter scope semantics
- assignment-expression target semantics inside comprehensions
- public dead-code candidates or findings

Part 1B fixtures cover decorators, defaults, class bases and keywords, class bodies, methods,
private-name mangling, lambdas, comprehension scope isolation, global and nonlocal declarations,
free-variable lookup, nested class lookup, invalid declarations, deterministic JSON snapshots, and
CPython `symtable` comparisons for selected lexical classifications.

Verification:

```bash
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo check --manifest-path fuzz/Cargo.toml
git diff --check
```
