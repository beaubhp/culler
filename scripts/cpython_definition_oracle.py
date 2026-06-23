#!/usr/bin/env python3
"""Emit CPython top-level definition facts for Part 0 parser checks."""

from __future__ import annotations

import ast
import json
import sys
import tokenize
from pathlib import Path


def line_starts_utf8(text: str) -> list[int]:
    starts = [0]
    offset = 0
    for line in text.splitlines(keepends=True):
        offset += len(line.encode("utf-8"))
        starts.append(offset)
    return starts


def absolute_range(node: ast.AST, starts: list[int]) -> dict[str, int]:
    return {
        "start": starts[node.lineno - 1] + node.col_offset,
        "end": starts[node.end_lineno - 1] + node.end_col_offset,
    }


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: cpython_definition_oracle.py PATH", file=sys.stderr)
        return 2

    path = Path(sys.argv[1])
    with tokenize.open(path) as file:
        text = file.read()

    starts = line_starts_utf8(text)
    module = ast.parse(text, filename=str(path))
    definitions = []
    for statement in module.body:
        if isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            kind = "class" if isinstance(statement, ast.ClassDef) else "function"
            definitions.append(
                {
                    "kind": kind,
                    "name": statement.name,
                    "range": absolute_range(statement, starts),
                    "is_async": isinstance(statement, ast.AsyncFunctionDef),
                }
            )

    print(json.dumps({"definitions": definitions}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
