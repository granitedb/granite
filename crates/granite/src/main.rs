//! The Granite server and its tools in one binary.
//!
//! Every tool is a subcommand, so a tool always matches the server it ships with.

use granite_storage::layout;
use granite_storage::superblock::{self, Identity};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: granite <command> [options]

Commands:
  format     Create a data file for one replica:
             granite format --cluster <id> --group <id> --replica <index> <path>
  start      Run a node
  repl       Open an interactive client
  bench      Run the benchmark workloads against a cluster
  version    Print the version
  help       Print this message
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };

    if command == "format" {
        return format(args.collect());
    }
    let has_extra_arguments = args.next().is_some();

    match command.as_str() {
        "version" | "--version" | "-V" | "help" | "--help" | "-h" if has_extra_arguments => {
            eprintln!("granite {command}: unexpected arguments");
            ExitCode::from(2)
        }
        "version" | "--version" | "-V" => {
            println!("granite {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        "start" | "repl" | "bench" => {
            eprintln!("granite {command}: not implemented yet");
            ExitCode::from(2)
        }
        other => {
            eprintln!("granite: unknown command '{other}'\n");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// `granite format`: creates the data file with its fixed zones and an empty
/// grid, and writes the initial superblock. Refuses, changing nothing, if
/// anything exists at the path.
fn format(args: Vec<String>) -> ExitCode {
    let Some((identity, path)) = format_arguments(&args) else {
        eprintln!("granite format: expected --cluster <id> --group <id> --replica <index> <path>");
        return ExitCode::from(2);
    };
    let head = superblock::format(identity);
    match granite_io::file::create(Path::new(path), layout::file_size(0), &head) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("granite format: {path}: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The identity and the path, if `args` are the options of `format`, each
/// once and in any order, then the path.
fn format_arguments(args: &[String]) -> Option<(Identity, &str)> {
    let (path, options) = args.split_last()?;
    let (mut cluster, mut group, mut replica) = (None, None, None);
    for pair in options.chunks(2) {
        let [name, value] = pair else { return None };
        match name.as_str() {
            "--cluster" if cluster.is_none() => cluster = Some(value.parse().ok()?),
            "--group" if group.is_none() => group = Some(value.parse().ok()?),
            "--replica" if replica.is_none() => replica = Some(value.parse().ok()?),
            _ => return None,
        }
    }
    let identity = Identity {
        cluster: cluster?,
        group: group?,
        replica: replica?,
    };
    Some((identity, path))
}
