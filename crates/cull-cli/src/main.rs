use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand, ValueEnum};
use cull_core::{
    CheckOutput, DefinitionKind, Diagnostic, DiagnosticSeverity, FindingConfidence, ProjectMode,
    PythonVersion,
};
use cull_python::{
    analyze_check, analyze_debug_bindings, analyze_debug_definitions, analyze_debug_references,
    CheckOptions, DebugBindingsOptions, DebugDefinitionsOptions, DebugReferencesOptions,
};

#[derive(Debug, Parser)]
#[command(name = "cull")]
#[command(about = "A precise Python dead-code analyzer.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Check {
        path: PathBuf,
        #[arg(long = "src")]
        source_roots: Vec<PathBuf>,
        #[arg(long, default_value = "text")]
        format: OutputFormat,
        #[arg(long)]
        target_python: Option<PythonVersion>,
        #[arg(long, value_enum)]
        mode: Option<CliProjectMode>,
    },
    #[command(hide = true)]
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
    },
}

#[derive(Debug, Subcommand)]
enum DebugCommand {
    Definitions {
        path: PathBuf,
        #[arg(long = "src")]
        source_roots: Vec<PathBuf>,
        #[arg(long, default_value = "json")]
        format: OutputFormat,
        #[arg(long)]
        target_python: Option<PythonVersion>,
    },
    Bindings {
        path: PathBuf,
        #[arg(long = "src")]
        source_roots: Vec<PathBuf>,
        #[arg(long, default_value = "json")]
        format: OutputFormat,
        #[arg(long)]
        target_python: Option<PythonVersion>,
    },
    References {
        path: PathBuf,
        #[arg(long = "src")]
        source_roots: Vec<PathBuf>,
        #[arg(long, default_value = "json")]
        format: OutputFormat,
        #[arg(long)]
        target_python: Option<PythonVersion>,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Clone, Debug, ValueEnum)]
enum CliProjectMode {
    Auto,
    Application,
    Library,
}

impl From<CliProjectMode> for ProjectMode {
    fn from(mode: CliProjectMode) -> Self {
        match mode {
            CliProjectMode::Auto => Self::Auto,
            CliProjectMode::Application => Self::Application,
            CliProjectMode::Library => Self::Library,
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let cli = Cli::parse();
    match cli.command {
        Command::Check {
            path,
            source_roots,
            format,
            target_python,
            mode,
        } => {
            let output = analyze_check(CheckOptions {
                project_root: path,
                source_roots,
                target_python,
                mode: mode.map(Into::into),
            })
            .map_err(|diagnostic| format_diagnostic(&diagnostic))?;

            let has_errors = output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error);
            match format {
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&output).map_err(|error| error.to_string())?
                    );
                }
                OutputFormat::Text => render_text_output(&output),
            }

            if matches!(format, OutputFormat::Text) {
                for diagnostic in &output.diagnostics {
                    eprintln!("{}", format_diagnostic(diagnostic));
                }
            }

            return Ok(if has_errors {
                ExitCode::from(2)
            } else if output.summary.high_confidence > 0 {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            });
        }
        Command::Debug { command } => match command {
            DebugCommand::Bindings {
                path,
                source_roots,
                format,
                target_python,
            } => {
                let output = analyze_debug_bindings(DebugBindingsOptions {
                    project_root: path,
                    source_roots,
                    target_python,
                })
                .map_err(|diagnostic| diagnostic.message)?;

                match format {
                    OutputFormat::Text => {
                        return Err("debug bindings only supports --format json".to_owned());
                    }
                    OutputFormat::Json => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&output)
                                .map_err(|error| error.to_string())?
                        );
                    }
                }
            }
            DebugCommand::Definitions {
                path,
                source_roots,
                format,
                target_python,
            } => {
                let output = analyze_debug_definitions(DebugDefinitionsOptions {
                    project_root: path,
                    source_roots,
                    target_python,
                })
                .map_err(|diagnostic| diagnostic.message)?;

                match format {
                    OutputFormat::Text => {
                        return Err("debug definitions only supports --format json".to_owned());
                    }
                    OutputFormat::Json => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&output)
                                .map_err(|error| error.to_string())?
                        );
                    }
                }
            }
            DebugCommand::References {
                path,
                source_roots,
                format,
                target_python,
            } => {
                let output = analyze_debug_references(DebugReferencesOptions {
                    project_root: path,
                    source_roots,
                    target_python,
                })
                .map_err(|diagnostic| diagnostic.message)?;

                match format {
                    OutputFormat::Text => {
                        return Err("debug references only supports --format json".to_owned());
                    }
                    OutputFormat::Json => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&output)
                                .map_err(|error| error.to_string())?
                        );
                    }
                }
            }
        },
    }
    Ok(ExitCode::SUCCESS)
}

fn render_text_output(output: &CheckOutput) {
    let mut first = true;
    for finding in output
        .findings
        .iter()
        .filter(|finding| finding.confidence == FindingConfidence::High)
    {
        if !first {
            println!();
        }
        first = false;
        println!(
            "{}:{}:{} {} {}",
            finding.definition.file,
            finding.definition.line,
            finding.definition.column,
            finding.rule_id.code(),
            finding.rule_id.text_name()
        );
        println!(
            "{} `{}` has no resolved inbound references under Cull's static model.",
            definition_label(finding.definition.kind),
            finding.definition.name
        );
        println!();
        println!("Confidence: high");
        println!();
        println!("Evidence:");
        for detail in &finding.explanation {
            println!("- {detail}");
        }
        for uncertainty in &finding.uncertainty {
            println!("- uncertainty: {}", uncertainty.detail);
        }
    }
}

fn definition_label(kind: DefinitionKind) -> &'static str {
    match kind {
        DefinitionKind::Function => "Function",
        DefinitionKind::Class => "Class",
    }
}

fn format_diagnostic(diagnostic: &Diagnostic) -> String {
    let location = match (&diagnostic.path, &diagnostic.range) {
        (Some(path), Some(range)) => format!("{path}:{}:{}", range.start, range.end),
        (Some(path), None) => path.clone(),
        (None, Some(range)) => format!("{}:{}", range.start, range.end),
        (None, None) => "<project>".to_owned(),
    };
    format!(
        "{location} {} {:?}: {}",
        diagnostic.code, diagnostic.severity, diagnostic.message
    )
}
