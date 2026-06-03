use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "marsala",
    version,
    about = "Phase 0 local-first Rust proxy spine"
)]
pub struct Cli {
    #[arg(
        long,
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
    #[command(about = "Start the Phase 0 healthz server and event log writer")]
    #[default]
    Serve,
    #[command(about = "Inspect merged configuration")]
    Config(ConfigArgs),
    #[command(about = "Read JSONL event logs")]
    Logs(LogsArgs),
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
pub struct PrintArgs {
    #[arg(
        long,
        default_value_t = false,
        help = "Include roadmap config sections that are inactive in Phase 0"
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
