use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand, ValueEnum};
use cull_core::PythonVersion;
use cull_python::{analyze_debug_definitions, DebugDefinitionsOptions};

#[derive(Debug, Parser)]
#[command(name = "cull")]
#[command(about = "A precise Python dead-code analyzer.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
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
}

#[derive(Clone, Debug, ValueEnum)]
enum OutputFormat {
    Json,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command {
        Command::Debug { command } => match command {
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
    Ok(())
}
