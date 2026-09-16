//! binit — move files to the Windows Recycle Bin, or refuse.
//!
//! Orchestration only: no COM and no `unsafe` live in this file.

// `unsafe` is opt-in per module: `com.rs`, `recycle.rs`, `sink.rs`, and
// `paths.rs` each `#![allow]` it at the top, as does the generated
// `bindings.rs`. Everything else is denied.
#![deny(unsafe_code)]

mod bindings;
mod com;
mod paths;
mod recycle;
mod report;
mod sink;

use std::ffi::OsString;

use clap::Parser;

use paths::{Prepared, PreparedPath};
use report::{EXIT_FATAL, EmitOptions, ErrorCode, Failed, Recycled, Report, Skipped, Style};
use sink::ItemOutcome;

const HELP_TEMPLATE: &str = "\
{about}

USAGE
  {usage}

OPTIONS
{options}{after-help}";

const AFTER_HELP: &str = "\
EXAMPLES
  binit unicorn.png rainbow.png
  binit *.log
  binit --dry-run build\\
  binit --json old-data.csv

binit never deletes permanently. If an item cannot be moved to the
Recycle Bin, binit refuses and exits non-zero. There is no --force.";

#[derive(Parser, Debug)]
#[command(
    name = "binit",
    version,
    about = "binit — move files to the Windows Recycle Bin",
    help_template = HELP_TEMPLATE,
    after_help = AFTER_HELP,
    override_usage = "binit <path|glob>...",
    disable_help_subcommand = true,
    disable_help_flag = true,
    disable_version_flag = true
)]
struct Cli {
    // OsString so a non-UTF-8 name is a lookup failure, not a usage error.
    #[arg(value_name = "path", required = true)]
    paths: Vec<OsString>,

    /// Print each item trashed
    #[arg(short = 'v', long)]
    verbose: bool,

    /// Show what would be trashed; change nothing
    #[arg(short = 'n', long = "dry-run")]
    dry_run: bool,

    /// Suppress the summary line
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Machine-readable output on stdout
    #[arg(long)]
    json: bool,

    /// Disable color
    #[arg(long = "no-color")]
    no_color: bool,

    /// Show this help
    #[arg(short = 'h', long = "help", action = clap::ArgAction::Help)]
    help: Option<bool>,

    /// Show version
    #[arg(short = 'V', long = "version", action = clap::ArgAction::Version)]
    version: Option<bool>,

    // rm-compat no-ops, so `binit -rf build\` works from muscle memory.
    // `-f` is free precisely because there is no --force.
    #[arg(short = 'r', hide = true)]
    _compat_r: bool,
    #[arg(short = 'R', hide = true)]
    _compat_upper_r: bool,
    #[arg(short = 'f', hide = true)]
    _compat_f: bool,
    #[arg(short = 'i', hide = true)]
    _compat_i: bool,
    #[arg(short = 'd', hide = true)]
    _compat_d: bool,
    #[arg(short = 'P', hide = true)]
    _compat_p: bool,
    #[arg(short = 'W', hide = true)]
    _compat_w: bool,
}

fn main() {
    // `run` must fully return before exiting: process::exit skips destructors,
    // and the COM guard has to unwind normally.
    let code = run();
    std::process::exit(code);
}

fn run() -> i32 {
    let cli = Cli::parse_from(wild::args_os());
    let style = Style::detect(cli.no_color);
    let opts = EmitOptions {
        verbose: cli.verbose,
        quiet: cli.quiet,
        json: cli.json,
        style,
    };

    let inputs: Vec<String> = cli
        .paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    let mut report = Report::new(cli.dry_run);
    let mut ready: Vec<PreparedPath> = Vec::new();

    for entry in paths::prepare(&inputs) {
        match entry {
            Prepared::Ready(p) => ready.push(p),
            Prepared::Skip {
                input,
                reason,
                container,
            } => report.skipped.push(Skipped {
                path: input,
                reason,
                container,
            }),
            Prepared::Fail {
                input,
                resolved,
                code,
                message,
            } => report.failed.push(Failed {
                path: Some(input),
                resolved,
                code,
                hresult: None,
                message,
            }),
        }
    }

    if cli.dry_run {
        for item in &ready {
            report.recycled.push(Recycled {
                path: item.input.clone(),
                resolved: item.resolved.clone(),
            });
        }
    } else if !ready.is_empty() {
        match recycle::recycle(&ready) {
            Ok(results) => {
                for (index, outcome) in results {
                    let Some(item) = ready.iter().find(|p| p.index == index) else {
                        continue;
                    };
                    record(&mut report, item, outcome);
                }
            }
            Err(e) => {
                report::emit(&fatal_report(cli.dry_run, &e), &opts);
                return EXIT_FATAL;
            }
        }
    }

    report::emit(&report, &opts);
    report.exit_code()
}

fn record(report: &mut Report, item: &PreparedPath, outcome: ItemOutcome) {
    match outcome {
        ItemOutcome::Recycled => report.recycled.push(Recycled {
            path: item.input.clone(),
            resolved: item.resolved.clone(),
        }),
        ItemOutcome::Vetoed => report.failed.push(Failed {
            path: Some(item.input.clone()),
            resolved: Some(item.resolved.clone()),
            code: ErrorCode::NotRecyclable,
            hresult: None,
            message: "the Recycle Bin is not available for this location".into(),
        }),
        ItemOutcome::Failed(hr) => {
            let (code, message) = report::classify(hr);
            report.failed.push(Failed {
                path: Some(item.input.clone()),
                resolved: Some(item.resolved.clone()),
                code,
                hresult: Some(hr),
                message,
            });
        }
        ItemOutcome::NoResult => report.failed.push(Failed {
            path: Some(item.input.clone()),
            resolved: Some(item.resolved.clone()),
            code: ErrorCode::NoResult,
            hresult: None,
            message: "the shell reported no outcome for this item".into(),
        }),
    }
}

fn fatal_report(dry_run: bool, error: &windows_core::Error) -> Report {
    let mut report = Report::new(dry_run);
    report.failed.push(Failed {
        path: None,
        resolved: None,
        code: ErrorCode::ComInitFailed,
        hresult: Some(error.code().0),
        message: format!(
            "could not initialize the Windows shell: {}. Nothing was deleted.",
            error.message().trim()
        ),
    });
    report
}
