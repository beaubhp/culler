#!/usr/bin/env python3
"""Emit CPython top-level definition facts for parser checks."""

from __future__ import annotations

import ast
import json
import symtable
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


def symbol_facts(table: symtable.SymbolTable) -> list[dict[str, object]]:
    symbols = []
    for name in sorted(table.get_identifiers()):
        symbol = table.lookup(name)
        symbols.append(
            {
                "name": name,
                "is_assigned": symbol.is_assigned(),
                "is_declared_global": symbol.is_declared_global(),
                "is_free": symbol.is_free(),
                "is_global": symbol.is_global(),
                "is_imported": symbol.is_imported(),
                "is_local": symbol.is_local(),
                "is_namespace": symbol.is_namespace(),
                "is_nonlocal": symbol.is_nonlocal(),
                "is_parameter": symbol.is_parameter(),
                "is_referenced": symbol.is_referenced(),
            }
        )
    return symbols


def child_facts(table: symtable.SymbolTable) -> list[dict[str, object]]:
    return [
        {
            "name": child.get_name(),
            "type": child.get_type(),
            "line": child.get_lineno(),
        }
        for child in sorted(
            table.get_children(),
            key=lambda child: (child.get_lineno(), child.get_name(), child.get_type()),
        )
    ]


def flatten_tables(
    table: symtable.SymbolTable, qualified_name: str
) -> list[dict[str, object]]:
    rows = [
        {
            "qualified_name": qualified_name,
            "name": table.get_name(),
            "type": table.get_type(),
            "line": table.get_lineno(),
            "symbols": symbol_facts(table),
            "children": child_facts(table),
        }
    ]

    for child in sorted(
        table.get_children(),
        key=lambda child: (child.get_lineno(), child.get_name(), child.get_type()),
    ):
        child_qualified_name = f"{qualified_name}.{child.get_name()}@{child.get_lineno()}"
        rows.extend(flatten_tables(child, child_qualified_name))
    return rows


def symtable_facts(text: str, filename: str) -> dict[str, object]:
    table = symtable.symtable(text, filename, "exec")

    return {
        "symbols": symbol_facts(table),
        "children": child_facts(table),
        "tables": flatten_tables(table, "top"),
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

    print(
        json.dumps(
            {
                "definitions": definitions,
                "symtable": symtable_facts(text, str(path)),
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
