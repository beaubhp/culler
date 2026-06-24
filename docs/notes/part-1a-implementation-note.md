# Part 1A Implementation Note

Document type: implementation note
Status: Part 1A complete
Audience: implementation and review
Last updated: 2026-06-24

## Summary

Part 1A adds deterministic semantic fact arenas and the hidden bindings debug command.

```bash
cull debug bindings path/to/project --format json
```

The command emits versioned JSON containing modules, module/function/class scopes, matching execution
contexts, symbols, ordered binding events, function/class definition associations, and diagnostics.
It does not emit reference facts, findings, reachability, import resolution, branch joins, annotation
phases, or flow-sensitive binding states.

## Model

The durable Part 1A model lives in `cull-core` and remains parser-neutral.

Implemented IDs:

- `ScopeId`
- `ContextId`
- `SymbolId`
- `BindingId`
- `BindingSetId`
- `ReferenceId`

Implemented facts:

- one module scope and module-body context per parsed module
- one function scope and function-body context per function definition
- one class scope and class-body context per class definition
- one `SymbolId` per `(ScopeId, name)`
- one `BindingId` per recorded binding event
- optional `DefId` association for function and class definition bindings
- `replaces` links to the preceding binding event for the same symbol in deterministic source order

Numeric IDs are snapshot-local analysis handles. They are deterministic for one repository snapshot
and are not promised stable across edits.

Scope parentage is lexical. Execution-context parentage is recorded separately. Class scopes are not
used as lexical parents for method bodies.

## Scope

Part 1A records binding inventory only. It intentionally does not implement:

- lexical reference resolution
- `debug references`
- branch or loop flow
- fixed points
- target-set widening
- annotation phase modeling
- public findings

## Verification

Part 1A verification includes:

- golden JSON snapshot for `cull debug bindings`
- deterministic repeated-output check
- repeated-definition and assignment-replacement test
- arena invariant test:
  - every `BindingId` belongs to exactly one `SymbolId`
  - every `SymbolId` belongs to exactly one `ScopeId`
  - every reportable `DefId` is associated with exactly one definition binding
  - arena and JSON ordering remain deterministic
