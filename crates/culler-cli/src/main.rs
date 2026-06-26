use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand, ValueEnum};
use culler_core::{
    Candidate, CandidateSummary, CheckAnalysis, CheckOutput, DefinitionKind, Diagnostic,
    DiagnosticSeverity, ExplainOutput, ExplainResult, FindingConfidence, FindingSubject,
    FindingType, ProjectCompleteness, ProjectMode, PythonVersion, RootCoverage,
};
use culler_python::{
    analyze_check, analyze_debug_bindings, analyze_debug_candidates, analyze_debug_definitions,
    analyze_debug_references, CheckOptions, DebugBindingsOptions, DebugCandidatesOptions,
    DebugDefinitionsOptions, DebugReferencesOptions,
};

#[derive(Debug, Parser)]
#[command(name = "culler")]
#[command(version)]
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
        #[arg(long)]
        show_review: bool,
        #[arg(long)]
        allow_partial: bool,
    },
    Explain {
        selector: String,
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long = "src")]
        source_roots: Vec<PathBuf>,
        #[arg(long, default_value = "text")]
        format: OutputFormat,
        #[arg(long)]
        target_python: Option<PythonVersion>,
        #[arg(long, value_enum)]
        mode: Option<CliProjectMode>,
        #[arg(long)]
        allow_partial: bool,
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
    Candidates {
        path: PathBuf,
        #[arg(long = "src")]
        source_roots: Vec<PathBuf>,
        #[arg(long, default_value = "json")]
        format: OutputFormat,
        #[arg(long)]
        target_python: Option<PythonVersion>,
        #[arg(long, value_enum)]
        mode: Option<CliProjectMode>,
        #[arg(long)]
        allow_partial: bool,
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
            show_review,
            allow_partial,
        } => {
            let output = match analyze_check(CheckOptions {
                project_root: path,
                source_roots,
                target_python,
                mode: mode.map(Into::into),
                allow_partial,
            }) {
                Ok(output) => output,
                Err(diagnostic) => {
                    if matches!(format, OutputFormat::Json) {
                        let output = error_check_output(diagnostic);
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&output)
                                .map_err(|error| error.to_string())?
                        );
                        return Ok(ExitCode::from(2));
                    }
                    return Err(format_diagnostic(&diagnostic));
                }
            };

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
                OutputFormat::Text => render_text_output(&output, show_review),
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
        Command::Explain {
            selector,
            path,
            source_roots,
            format,
            target_python,
            mode,
            allow_partial,
        } => {
            let requested_selector = selector.clone();
            let output = match explain_selector(
                selector,
                path,
                source_roots,
                target_python,
                mode.map(Into::into),
                allow_partial,
            ) {
                Ok(output) => output,
                Err(diagnostic) => {
                    if matches!(format, OutputFormat::Json) {
                        let output = error_explain_output(requested_selector, diagnostic);
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&output)
                                .map_err(|error| error.to_string())?
                        );
                        return Ok(ExitCode::from(2));
                    }
                    return Err(format_diagnostic(&diagnostic));
                }
            };
            let success = matches!(output.result, ExplainResult::Found { .. });
            match format {
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&output).map_err(|error| error.to_string())?
                    );
                }
                OutputFormat::Text => render_explain_output(&output),
            }
            return Ok(if success {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
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
            DebugCommand::Candidates {
                path,
                source_roots,
                format,
                target_python,
                mode,
                allow_partial,
            } => {
                let output = analyze_debug_candidates(DebugCandidatesOptions {
                    project_root: path,
                    source_roots,
                    target_python,
                    mode: mode.map(Into::into),
                    allow_partial,
                })
                .map_err(|diagnostic| diagnostic.message)?;

                match format {
                    OutputFormat::Text => {
                        return Err("debug candidates only supports --format json".to_owned());
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

fn render_text_output(output: &CheckOutput, show_review: bool) {
    let mut first = true;
    for finding in output
        .findings
        .iter()
        .filter(|finding| show_review || finding.confidence == FindingConfidence::High)
    {
        if !first {
            println!();
        }
        first = false;
        println!(
            "{}:{}:{} {} {}",
            finding.subject.file(),
            finding.subject.line(),
            finding.subject.column(),
            finding.rule_id.code(),
            finding.rule_id.text_name()
        );
        println!("{}", finding_summary(finding));
        println!();
        println!("Confidence: {}", confidence_label(finding.confidence));
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

fn explain_selector(
    selector: String,
    path: PathBuf,
    source_roots: Vec<PathBuf>,
    target_python: Option<PythonVersion>,
    mode: Option<ProjectMode>,
    allow_partial: bool,
) -> Result<ExplainOutput, Diagnostic> {
    let output = analyze_debug_candidates(DebugCandidatesOptions {
        project_root: path,
        source_roots,
        target_python,
        mode,
        allow_partial,
    })?;
    let exact = output
        .candidates
        .iter()
        .find(|candidate| candidate.candidate_id == selector)
        .cloned();
    let result = if let Some(candidate) = exact {
        ExplainResult::Found {
            candidate: Box::new(candidate),
        }
    } else {
        let matches = output
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.subject_id == selector
                    || candidate.subject.qualified_name() == selector
                    || candidate.subject.name() == selector
                    || subject_module_name(&candidate.subject)
                        .is_some_and(|module_name| module_name == selector)
            })
            .map(candidate_summary)
            .collect::<Vec<_>>();
        match matches.len() {
            0 => ExplainResult::NotFound,
            1 => {
                let id = matches[0].candidate_id.clone();
                if let Some(candidate) = output
                    .candidates
                    .iter()
                    .find(|candidate| candidate.candidate_id == id)
                    .cloned()
                {
                    ExplainResult::Found {
                        candidate: Box::new(candidate),
                    }
                } else {
                    ExplainResult::NotFound
                }
            }
            _ => ExplainResult::Ambiguous {
                candidates: matches,
            },
        }
    };
    Ok(ExplainOutput {
        schema_version: output.schema_version,
        selector,
        analysis: output.analysis,
        project_root: output.project_root,
        project_completeness: output.project_completeness,
        result,
        diagnostics: output.diagnostics,
    })
}

fn candidate_summary(candidate: &Candidate) -> CandidateSummary {
    CandidateSummary {
        candidate_id: candidate.candidate_id.clone(),
        rule_id: candidate.rule_id,
        status: candidate.status,
        confidence: candidate.confidence,
        qualified_name: candidate.subject.qualified_name(),
        file: candidate.subject.file().to_owned(),
        line: candidate.subject.line(),
        column: candidate.subject.column(),
    }
}

fn render_explain_output(output: &ExplainOutput) {
    match &output.result {
        ExplainResult::Found { candidate } => render_candidate_explanation(candidate),
        ExplainResult::Ambiguous { candidates } => {
            println!("selector `{}` is ambiguous", output.selector);
            println!();
            for candidate in candidates {
                println!(
                    "{} {} {:?} {} {}:{}:{}",
                    candidate.candidate_id,
                    candidate.rule_id.code(),
                    candidate.status,
                    candidate.qualified_name,
                    candidate.file,
                    candidate.line,
                    candidate.column
                );
            }
        }
        ExplainResult::NotFound => {
            println!("selector `{}` did not match any candidate", output.selector);
        }
    }
}

fn render_candidate_explanation(candidate: &Candidate) {
    println!(
        "{}:{}:{} {} {}",
        candidate.subject.file(),
        candidate.subject.line(),
        candidate.subject.column(),
        candidate.rule_id.code(),
        candidate.rule_id.text_name()
    );
    println!("{}", finding_type_summary(candidate));
    println!();
    println!("Candidate ID: {}", candidate.candidate_id);
    println!("Status: {:?}", candidate.status);
    if let Some(confidence) = candidate.confidence {
        println!("Confidence: {:?}", confidence);
    }
    if !candidate.blockers.is_empty() {
        println!();
        println!("Blockers:");
        for blocker in &candidate.blockers {
            println!("- {:?}: {}", blocker.kind, blocker.detail);
        }
    }
    if !candidate.suppression_reasons.is_empty() {
        println!();
        println!("Suppression:");
        for reason in &candidate.suppression_reasons {
            println!("- {:?}: {}", reason.kind, reason.detail);
        }
    }
    println!();
    println!("Evidence:");
    for evidence in &candidate.evidence {
        println!("- {:?}: {}", evidence.kind, evidence.summary);
    }
}

fn finding_type_summary(candidate: &Candidate) -> String {
    subject_finding_summary(candidate.finding_type, &candidate.subject)
}

fn error_check_output(diagnostic: Diagnostic) -> CheckOutput {
    CheckOutput {
        schema_version: 3,
        analysis: CheckAnalysis {
            mode: ProjectMode::Auto,
            target_python: PythonVersion::default(),
            root_coverage: RootCoverage::Absent,
        },
        project_completeness: ProjectCompleteness::complete(),
        target_python: PythonVersion::default(),
        project_root: ".".to_owned(),
        source_roots: Vec::new(),
        mode: ProjectMode::Auto,
        root_coverage: RootCoverage::Absent,
        roots: Vec::new(),
        findings: Vec::new(),
        summary: culler_core::CheckSummary::default(),
        diagnostics: vec![diagnostic],
    }
}

fn error_explain_output(selector: String, diagnostic: Diagnostic) -> ExplainOutput {
    ExplainOutput {
        schema_version: 3,
        selector,
        analysis: CheckAnalysis {
            mode: ProjectMode::Auto,
            target_python: PythonVersion::default(),
            root_coverage: RootCoverage::Absent,
        },
        project_root: ".".to_owned(),
        project_completeness: ProjectCompleteness::complete(),
        result: ExplainResult::NotFound,
        diagnostics: vec![diagnostic],
    }
}

fn definition_label(kind: DefinitionKind) -> &'static str {
    match kind {
        DefinitionKind::Function => "Function",
        DefinitionKind::Class => "Class",
    }
}

fn subject_label(subject: &FindingSubject) -> &'static str {
    match subject {
        FindingSubject::Definition { definition } => definition_label(definition.kind),
        FindingSubject::Binding { .. } => "Binding",
        FindingSubject::ImportBinding { .. } => "Import binding",
        FindingSubject::StatementRange { .. } => "Statement range",
    }
}

fn subject_kind_label(subject: &FindingSubject) -> &'static str {
    match subject.definition() {
        Some(definition) => definition_label(definition.kind),
        None => subject_label(subject),
    }
}

fn subject_module_name(subject: &FindingSubject) -> Option<String> {
    match subject {
        FindingSubject::Definition { definition } => {
            Some(format!("{}::{}", definition.module, definition.name))
        }
        FindingSubject::Binding { binding } => {
            Some(format!("{}::{}", binding.module, binding.name))
        }
        FindingSubject::ImportBinding { import_binding } => Some(format!(
            "{}::{}",
            import_binding.module, import_binding.bound_name
        )),
        FindingSubject::StatementRange { .. } => None,
    }
}

fn confidence_label(confidence: FindingConfidence) -> &'static str {
    match confidence {
        FindingConfidence::High => "high",
        FindingConfidence::Review => "review",
    }
}

fn finding_summary(finding: &culler_core::Finding) -> String {
    subject_finding_summary(finding.finding_type, &finding.subject)
}

fn subject_finding_summary(finding_type: FindingType, subject: &FindingSubject) -> String {
    match finding_type {
        FindingType::Unreferenced => format!(
            "{} `{}` has no resolved inbound references under Culler's static model.",
            subject_kind_label(subject),
            subject.name()
        ),
        FindingType::RootUnreachable => format!(
            "{} `{}` has no runtime path from Culler's recognized roots.",
            subject_kind_label(subject),
            subject.name()
        ),
        FindingType::UnusedImport => format!(
            "Import binding `{}` has no may-execute references to the exact binding.",
            subject.name()
        ),
        FindingType::UnusedLocalBinding => format!(
            "Local binding `{}` has no may-execute reads of the exact binding.",
            subject.name()
        ),
        FindingType::UnreachableStatement => {
            "Statement range cannot execute after a preceding terminal statement.".to_owned()
        }
        FindingType::UnusedPrivateMethod => format!(
            "Private method `{}` has no resolved method references under Culler's static model.",
            subject.name()
        ),
    }
}

fn format_diagnostic(diagnostic: &Diagnostic) -> String {
    let location = match (&diagnostic.path, &diagnostic.range) {
        (Some(path), Some(range)) => format!("{path}:bytes {}..{}", range.start, range.end),
        (Some(path), None) => path.clone(),
        (None, Some(range)) => format!("bytes {}..{}", range.start, range.end),
        (None, None) => "<project>".to_owned(),
    };
    format!(
        "{location} {} {:?}: {}",
        diagnostic.code, diagnostic.severity, diagnostic.message
    )
}
