# Commit Message Guide

Write commit messages as Conventional Commits. Keep the subject specific,
behavioral, and short enough to scan in history.

Use this shape:

```text
<type>(optional-scope): <specific change>
```

Good examples:

```text
feat: detect unused private methods
fix: preserve exported helpers in library mode
perf: reuse import graph traversal
docs: document PyPI installation
ci: build release wheels with trusted publishing
```

Use `feat` for user-visible behavior, `fix` for correctness, `perf` for speed
or memory improvements, `docs` for documentation, `test` for tests, `refactor`
for internal cleanup, `build` for packaging, and `ci` for automation.

Avoid vague messages like `update`, `cleanup`, `misc`, or `final changes`.
If a change has multiple unrelated purposes, split it into separate commits.
