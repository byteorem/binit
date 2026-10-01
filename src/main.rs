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

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};

use paths::{Prepared, PreparedPath};
use report::{
    EXIT_FATAL, EmitOptions, ErrorCode, Failed, Recycled, Report, SkipReason, Skipped, Style,
};
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
  binit --files-from list.txt

binit never deletes permanently. If an item cannot be moved to the
Recycle Bin, binit refuses and exits non-zero. There is no --force.

-f, -r, -R and --recursive are accepted so rm habits work. Only -f does
anything: a path that does not exist is reported as skipped instead of
failing. It forces nothing, and every other failure still fails.";

#[derive(Parser, Debug)]
#[command(
    name = "binit",
    version,
    about = "binit — move files to the Windows Recycle Bin",
    help_template = HELP_TEMPLATE,
    after_help = AFTER_HELP,
    override_usage = "binit <path|glob>...\n  binit --files-from <file>",
    disable_help_subcommand = true,
    disable_help_flag = true,
    disable_version_flag = true
)]
struct Cli {
    // OsString so a non-UTF-8 name is a lookup failure, not a usage error.
    // Not `required`: `--files-from` can supply them instead, and `run` reports
    // a usage error when there are neither.
    #[arg(value_name = "path")]
    paths: Vec<OsString>,

    /// Read more paths from a file, one per line; `-` reads stdin. Lines are
    /// literal paths: no glob expansion
    #[arg(long = "files-from", value_name = "file")]
    files_from: Option<OsString>,

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

    // rm-compat flags, so `binit -rf build\` works from muscle memory.
    // `-f` is not a force flag: there is none. Like `rm -f` it only stops a
    // missing path from failing the run (it is reported as skipped), which
    // keeps idempotent cleanup working. It changes nothing else.
    #[arg(short = 'f', hide = true)]
    ignore_missing: bool,
    #[arg(short = 'r', long = "recursive", hide = true)]
    _compat_r: bool,
    #[arg(short = 'R', hide = true)]
    _compat_upper_r: bool,
    // Declared only so `run` can refuse it with an explanation; clap's stock
    // "unexpected argument" tip would suggest passing `--force` as a path.
    #[arg(long, hide = true)]
    force: bool,
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

    if cli.force {
        usage_error(
            ErrorKind::UnknownArgument,
            "binit has no --force: it never deletes permanently. To delete permanently, run Remove-Item yourself.",
        );
    }

    let mut inputs: Vec<String> = cli
        .paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if let Some(source) = &cli.files_from {
        match read_files_from(source) {
            Ok(lines) => inputs.extend(lines),
            Err(e) => usage_error(
                ErrorKind::Io,
                &format!("cannot read --files-from {}: {e}", source.to_string_lossy()),
            ),
        }
    } else if inputs.is_empty() {
        usage_error(
            ErrorKind::MissingRequiredArgument,
            "no paths given; pass paths or --files-from <file>",
        );
    }

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
                container: Some(container),
            }),
            Prepared::Fail {
                input,
                code: ErrorCode::NotFound,
                ..
            } if cli.ignore_missing => report.skipped.push(Skipped {
                path: input,
                reason: SkipReason::Missing,
                container: None,
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
                let by_index: HashMap<usize, &PreparedPath> =
                    ready.iter().map(|p| (p.index, p)).collect();
                for (index, outcome) in results {
                    let Some(item) = by_index.get(&index) else {
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

/// Print a clap-style usage error and exit 2. Nothing is alive yet that a
/// skipped destructor could matter to: this runs before any COM call.
fn usage_error(kind: ErrorKind, message: &str) -> ! {
    Cli::command().error(kind, message).exit()
}

/// Read a `--files-from` list: UTF-8, one literal path per line, blank lines
/// ignored. `-` is stdin, read only when asked for so binit never blocks on a
/// terminal by surprise.
fn read_files_from(source: &OsStr) -> std::io::Result<Vec<String>> {
    let text = if source == "-" {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        text
    } else {
        std::fs::read_to_string(source)?
    };
    // PowerShell 5 writes a BOM with `-Encoding UTF8`; it is not part of a path.
    Ok(text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.trim().is_empty())
        .map(String::from)
        .collect())
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
