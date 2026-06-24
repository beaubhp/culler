use cull_core::{
    DefId, DefinitionIr, DefinitionKey, DefinitionKind, Diagnostic, ModuleIr, PythonVersion,
    TextRange,
};
use ruff_python_ast::{Mod, ModModule, PythonVersion as RuffPythonVersion, Stmt};
use ruff_python_parser::{parse_unchecked, Mode, ParseOptions};
use ruff_text_size::TextRange as RuffTextRange;

use crate::frontend::{ParseInput, ParsedModule, PythonFrontend};

#[derive(Default)]
pub struct RuffFrontend;

impl PythonFrontend for RuffFrontend {
    fn parse_module(&self, input: ParseInput<'_>) -> Result<ParsedModule, Vec<Diagnostic>> {
        let module = parse_ruff_module(input)?;
        let future_annotations = has_future_annotations(&module.body);
        let definitions = module
            .body
            .iter()
            .filter_map(|statement| lower_definition(statement, input))
            .enumerate()
            .map(|(index, mut definition)| {
                definition.id = DefId::new(index as u32);
                definition
            })
            .collect();

        Ok(ParsedModule {
            module: ModuleIr {
                id: input.module_id,
                file: input.file_id,
                name: input.module_name.to_owned(),
                path: input.display_path.to_owned(),
                future_annotations,
                definitions,
            },
            diagnostics: Vec::new(),
        })
    }
}

pub(crate) fn parse_ruff_module(input: ParseInput<'_>) -> Result<ModModule, Vec<Diagnostic>> {
    let options =
        ParseOptions::from(Mode::Module).with_target_version(to_ruff_version(input.target_python));
    let parsed = parse_unchecked(&input.source.text, options);

    let diagnostics = parser_diagnostics(input.display_path, &parsed);
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    let Mod::Module(module) = parsed.into_syntax() else {
        return Err(vec![Diagnostic::error(
            "CULL_P0200",
            "parser did not return a module",
        )
        .with_path(input.display_path.to_owned())]);
    };

    Ok(module)
}

fn parser_diagnostics<T>(path: &str, parsed: &ruff_python_parser::Parsed<T>) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for error in parsed.errors() {
        diagnostics.push(
            Diagnostic::error("CULL_P0201", error.error.to_string())
                .with_path(path.to_owned())
                .with_range(to_range(error.location)),
        );
    }
    for error in parsed.unsupported_syntax_errors() {
        diagnostics.push(
            Diagnostic::error("CULL_P0202", error.to_string())
                .with_path(path.to_owned())
                .with_range(to_range(error.range)),
        );
    }
    diagnostics
}

fn lower_definition(statement: &Stmt, input: ParseInput<'_>) -> Option<DefinitionIr> {
    match statement {
        Stmt::FunctionDef(function) => {
            let name_range = to_range(function.name.range);
            let range = definition_statement_range(
                &input.source.text,
                to_range(function.range),
                name_range,
                function.is_async,
                DefinitionKind::Function,
            );
            let name = function.name.id.to_string();
            Some(DefinitionIr {
                id: DefId::new(0),
                key: DefinitionKey {
                    module: input.module_name.to_owned(),
                    kind: DefinitionKind::Function,
                    lexical_parent: None,
                    name: name.clone(),
                    range,
                },
                kind: DefinitionKind::Function,
                name,
                range,
                name_range,
                is_async: function.is_async,
                decorator_count: function.decorator_list.len(),
                type_parameter_count: function
                    .type_params
                    .as_ref()
                    .map(|params| params.type_params.len())
                    .unwrap_or(0),
            })
        }
        Stmt::ClassDef(class) => {
            let name_range = to_range(class.name.range);
            let range = definition_statement_range(
                &input.source.text,
                to_range(class.range),
                name_range,
                false,
                DefinitionKind::Class,
            );
            let name = class.name.id.to_string();
            Some(DefinitionIr {
                id: DefId::new(0),
                key: DefinitionKey {
                    module: input.module_name.to_owned(),
                    kind: DefinitionKind::Class,
                    lexical_parent: None,
                    name: name.clone(),
                    range,
                },
                kind: DefinitionKind::Class,
                name,
                range,
                name_range,
                is_async: false,
                decorator_count: class.decorator_list.len(),
                type_parameter_count: class
                    .type_params
                    .as_ref()
                    .map(|params| params.type_params.len())
                    .unwrap_or(0),
            })
        }
        _ => None,
    }
}

fn has_future_annotations(statements: &[Stmt]) -> bool {
    statements.iter().any(|statement| {
        let Stmt::ImportFrom(import) = statement else {
            return false;
        };
        import.level == 0
            && import
                .module
                .as_ref()
                .is_some_and(|module| module.id.as_str() == "__future__")
            && import
                .names
                .iter()
                .any(|alias| alias.name.id.as_str() == "annotations")
    })
}

pub(crate) fn module_has_future_annotations(statements: &[Stmt]) -> bool {
    has_future_annotations(statements)
}

pub(crate) fn to_range(range: RuffTextRange) -> TextRange {
    TextRange::new(range.start().to_u32(), range.end().to_u32())
}

pub(crate) fn definition_statement_range(
    source: &str,
    fallback: TextRange,
    name_range: TextRange,
    is_async: bool,
    kind: DefinitionKind,
) -> TextRange {
    let line_start = source[..name_range.start as usize]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let header = &source[line_start..name_range.start as usize];
    let keyword = match (kind, is_async) {
        (DefinitionKind::Function, true) => "async def",
        (DefinitionKind::Function, false) => "def",
        (DefinitionKind::Class, _) => "class",
    };
    let start = header
        .find(keyword)
        .map(|offset| (line_start + offset) as u32)
        .unwrap_or(fallback.start);
    TextRange::new(start, fallback.end)
}

fn to_ruff_version(version: PythonVersion) -> RuffPythonVersion {
    RuffPythonVersion {
        major: version.major,
        minor: version.minor,
    }
}

#[cfg(test)]
mod tests {
    use cull_core::{FileId, ModuleId};

    use crate::{decode_python_source, DecodedSource};

    use super::*;

    #[test]
    fn lowers_top_level_definitions_only() {
        let source = decode_python_source(
            b"from __future__ import annotations\n\n@dec\ndef f[T]():\n    def nested(): pass\n\nclass C: pass\n",
        )
        .unwrap();
        let parsed = RuffFrontend
            .parse_module(ParseInput {
                file_id: FileId::new(0),
                module_id: ModuleId::new(0),
                module_name: "acme.mod",
                display_path: "src/acme/mod.py",
                source: &source,
                target_python: PythonVersion::PY314,
            })
            .unwrap();

        assert!(parsed.module.future_annotations);
        assert_eq!(parsed.module.definitions.len(), 2);
        assert_eq!(parsed.module.definitions[0].name, "f");
        assert_eq!(parsed.module.definitions[0].type_parameter_count, 1);
        assert_eq!(parsed.module.definitions[1].name, "C");
    }

    #[test]
    fn syntax_errors_are_structured() {
        let source = DecodedSource {
            text: "def nope(:\n".to_owned(),
            info: cull_core::DecodedSourceInfo {
                encoding: "utf-8".to_owned(),
                had_utf8_bom: false,
            },
        };
        let errors = RuffFrontend
            .parse_module(ParseInput {
                file_id: FileId::new(0),
                module_id: ModuleId::new(0),
                module_name: "bad",
                display_path: "bad.py",
                source: &source,
                target_python: PythonVersion::PY314,
            })
            .unwrap_err();

        assert!(!errors.is_empty());
        assert_eq!(errors[0].code, "CULL_P0201");
    }

    #[test]
    fn parses_shared_modern_syntax_corpus() {
        let snippets = [
            "match value:\n    case {'x': y} if (z := y):\n        pass\n",
            "try:\n    pass\nexcept* ValueError as error:\n    pass\n",
            "type Response[T] = list[T]\n",
            "class Box[T]:\n    pass\n",
            "def collect[T](items: list[T]) -> list[T]:\n    return [item for item in items]\n",
            "message = f'{name=}'\n",
            "template = t'hello {name}'\n",
        ];

        for snippet in snippets {
            let source = decode_python_source(snippet.as_bytes()).unwrap();
            let parsed = RuffFrontend.parse_module(ParseInput {
                file_id: FileId::new(0),
                module_id: ModuleId::new(0),
                module_name: "syntax",
                display_path: "syntax.py",
                source: &source,
                target_python: PythonVersion::PY314,
            });
            assert!(parsed.is_ok(), "failed to parse snippet:\n{snippet}");
        }
    }

    #[test]
    fn target_version_errors_are_structured() {
        let source = decode_python_source(b"type Response[T] = list[T]\n").unwrap();
        let errors = RuffFrontend
            .parse_module(ParseInput {
                file_id: FileId::new(0),
                module_id: ModuleId::new(0),
                module_name: "syntax",
                display_path: "syntax.py",
                source: &source,
                target_python: PythonVersion::PY310,
            })
            .unwrap_err();

        assert!(errors.iter().any(|error| error.code == "CULL_P0202"));
    }
}
