//! The Granite server and its tools in one binary.
//!
//! Every tool is a subcommand, so a tool always matches the server it ships with.

use std::process::ExitCode;

const USAGE: &str = "\
Usage: granite <command> [options]

Commands:
  format     Create a data file for one replica
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
        "format" | "start" | "repl" | "bench" => {
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
