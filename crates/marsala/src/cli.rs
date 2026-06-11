use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "marsala",
    version,
    about = "Phase 1 local-first Rust OpenAI-compatible proxy spine"
)]
pub struct Cli {
    #[arg(
        long = "config",
        visible_alias = "config-path",
        global = true,
        env = "MARSALA_CONFIG",
        help = "Load configuration from this TOML file"
    )]
    pub config_path: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Clone, Default, Subcommand)]
pub enum Command {
    #[command(about = "Start the Phase 1 healthz and chat completions server")]
    #[default]
    Serve,
    #[command(about = "Inspect merged configuration")]
    Config(ConfigArgs),
    #[command(about = "Read JSONL event logs")]
    Logs(LogsArgs),
    #[command(about = "Manage allowlisted MITM foundation assets")]
    Mitm(MitmArgs),
    #[command(about = "Manage Codex interception integration")]
    Codex(CodexArgs),
}

#[derive(Debug, Clone, Args)]
pub struct CodexArgs {
    #[command(subcommand)]
    pub command: CodexCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum CodexCommand {
    #[command(about = "Manage terminal environment integration")]
    Env(CodexEnvArgs),
}

#[derive(Debug, Clone, Args)]
pub struct CodexEnvArgs {
    #[command(subcommand)]
    pub command: CodexEnvCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum CodexEnvCommand {
    #[command(about = "Install the marked Bash startup block")]
    Install(CodexEnvFileArgs),
    #[command(about = "Remove the marked Bash startup block")]
    Uninstall(CodexEnvFileArgs),
}

#[derive(Debug, Clone, Args)]
pub struct CodexEnvFileArgs {
    #[arg(long, help = "Shell startup file; defaults to ~/.bashrc")]
    pub shell_file: Option<PathBuf>,
    #[arg(long, help = "Marsala project directory containing certs/")]
    pub project_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ConfigCommand {
    #[command(about = "Print merged config as TOML")]
    Print(PrintArgs),
}

#[derive(Debug, Clone, Args)]
pub struct LogsArgs {
    #[command(subcommand)]
    pub command: LogsCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum LogsCommand {
    #[command(about = "Print recent log lines and optionally keep following")]
    Tail(TailArgs),
}

#[derive(Debug, Clone, Args)]
pub struct MitmArgs {
    #[command(subcommand)]
    pub command: MitmCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum MitmCommand {
    #[command(about = "Manage the local Marsala CA")]
    Ca(CaArgs),
}

#[derive(Debug, Clone, Args)]
pub struct CaArgs {
    #[command(subcommand)]
    pub command: CaCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum CaCommand {
    #[command(about = "Generate the local Marsala CA without overwriting existing files")]
    Init,
}

#[derive(Debug, Clone, Args)]
pub struct PrintArgs {
    #[arg(
        long,
        default_value_t = false,
        help = "Include roadmap config sections that are inactive after Phase 1"
    )]
    pub all: bool,
}

#[derive(Debug, Clone, Args)]
pub struct TailArgs {
    #[arg(
        long,
        default_value_t = 20,
        help = "Number of trailing lines to print first"
    )]
    pub lines: usize,

    #[arg(
        long,
        default_value_t = false,
        help = "Keep watching for appended and recreated log files"
    )]
    pub follow: bool,
}
