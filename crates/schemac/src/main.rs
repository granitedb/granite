//! `granite-schemac`: compiles a schema text file to the binary catalog, or
//! prints a binary catalog as canonical text.

use granite_schema::Catalog;
use std::io::Write;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: granite-schemac compile <schema file>   write the binary catalog to stdout
       granite-schemac print <catalog file>    print a binary catalog as schema text
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [command, path] = &args[..] else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "a command-line tool reads the one file it is given; the library reads nothing"
    )]
    let input = match std::fs::read(path) {
        Ok(input) => input,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let output = match command.as_str() {
        "compile" => granite_schemac::compile(path, &input).map_err(|e| e.to_string()),
        "print" => (Catalog::decode(&input))
            .map(|c| granite_schemac::print(&c).into_bytes())
            .map_err(|e| format!("{path}: not a valid catalog: {e:?} (code {})", e as u8)),
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match output {
        Ok(bytes) => match std::io::stdout().write_all(&bytes) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("stdout: {e}");
                ExitCode::FAILURE
            }
        },
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
