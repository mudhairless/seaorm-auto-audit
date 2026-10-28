//! `seaorm-auto-audit-check`: fail the build when audited tables are written in a way
//! that skips SeaORM's lifecycle hooks.
//!
//! ```text
//! seaorm-auto-audit-check [--allow <path>]... <root>...
//! ```
//!
//! Every `<root>` is a directory (or file) to scan. `--allow` exempts a
//! directory, a file, or a bare file name such as `audit.rs`; it may be
//! repeated.
//!
//! Exit code 0 means clean, 1 means at least one forbidden call site was
//! found, 2 means the arguments made no sense or a root could not be
//! scanned (unreadable, or nothing in it to scan).

use std::path::PathBuf;
use std::process::ExitCode;

use seaorm_auto_audit::scan::Checker;

const USAGE: &str = "usage: seaorm-auto-audit-check [--allow <path>]... <root>...";

fn main() -> ExitCode {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut checker = Checker::new();
    let mut args = std::env::args().skip(1);

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--allow" => match args.next() {
                Some(path) => {
                    checker.allow(path);
                }
                None => {
                    eprintln!("error: --allow needs a path\n{USAGE}");
                    return ExitCode::from(2);
                }
            },
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown option `{other}`\n{USAGE}");
                return ExitCode::from(2);
            }
            other => roots.push(PathBuf::from(other)),
        }
    }

    if roots.is_empty() {
        eprintln!("error: nothing to scan\n{USAGE}");
        return ExitCode::from(2);
    }

    // A root that does not exist scans as zero files and zero violations,
    // which would pass a gate while checking nothing. Refuse it the way
    // "nothing to scan" is refused above.
    for root in &roots {
        if !root.exists() {
            eprintln!("error: `{}` does not exist\n{USAGE}", root.display());
            return ExitCode::from(2);
        }
    }

    let mut violations = Vec::new();
    let mut unscannable = false;
    for root in &roots {
        match checker.scan(root) {
            Ok(found) => violations.extend(found),
            Err(error) => {
                eprintln!("error: {error}");
                unscannable = true;
            }
        }
    }
    if unscannable {
        return ExitCode::from(2);
    }

    if violations.is_empty() {
        return ExitCode::SUCCESS;
    }

    eprintln!(
        "seaorm-auto-audit-check: {} forbidden write site(s):",
        violations.len()
    );
    for violation in &violations {
        eprintln!("  {violation}");
    }
    eprintln!(
        "\nAudited tables are written with ActiveModel methods (insert, update, save, delete) \
         inside a transaction."
    );

    ExitCode::FAILURE
}
