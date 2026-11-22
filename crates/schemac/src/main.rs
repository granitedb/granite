//! `granite-schemac`: compiles a schema text file to the binary catalog, or
//! prints a binary catalog as canonical text; compiles a program text file
//! against a schema to the program IR, prints an IR as canonical text, or
//! reports what the validator found out about a program.

use granite_schema::Catalog;
use granite_txn::Program;
use std::collections::BTreeSet;
use std::io::Write;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: granite-schemac compile <schema file>                    write the binary catalog to stdout
       granite-schemac print <catalog file>                     print a binary catalog as schema text
       granite-schemac compile-program <schema file> <program>  write the program IR to stdout
       granite-schemac print-program <schema file> <IR file>    print a program IR as program text
       granite-schemac report <schema file> <program>           print the program's bounds, key provenance and shard set
";

#[expect(
    clippy::disallowed_methods,
    reason = "a command-line tool reads the files it is given; the library reads nothing"
)]
fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{path}: {e}"))
}

fn catalog(path: &str) -> Result<Catalog, String> {
    let bytes = granite_schemac::compile(path, &read(path)?).map_err(|e| e.to_string())?;
    Ok(Catalog::decode(&bytes).expect("a compiled catalog decodes"))
}

fn run(args: &[String]) -> Option<Result<Vec<u8>, String>> {
    let output = match args {
        [command, path] if command == "compile" => read(path)
            .and_then(|input| granite_schemac::compile(path, &input).map_err(|e| e.to_string())),
        [command, path] if command == "print" => read(path).and_then(|input| {
            (Catalog::decode(&input))
                .map(|c| granite_schemac::print(&c).into_bytes())
                .map_err(|e| format!("{path}: not a valid catalog: {e:?} (code {})", e as u8))
        }),
        [command, schema, path] if command == "compile-program" || command == "report" => (|| {
            let catalog = catalog(schema)?;
            let program = granite_schemac::compile_program(path, &read(path)?, &catalog)
                .map_err(|e| e.to_string())?;
            Ok(if command == "report" {
                granite_schemac::report(&catalog, &program).into_bytes()
            } else {
                program.encode().expect("a valid program encodes")
            })
        })(
        ),
        [command, schema, path] if command == "print-program" => (|| {
            let catalog = catalog(schema)?;
            let program = Program::decode(&read(path)?)
                .map_err(|e| format!("{path}: not a program IR: {e:?} (code {})", e as u8))?;
            program.validate(&catalog).map_err(|i| {
                let e = i.error;
                format!(
                    "{path}: not a valid program: {e:?} (code {}) at node {}",
                    e as u8, i.node
                )
            })?;
            let names: BTreeSet<&str> = program.params.iter().map(|p| p.name.as_str()).collect();
            if names.len() != program.params.len() {
                return Err(format!(
                    "{path}: two parameters have one name, so text cannot tell them apart"
                ));
            }
            Ok(granite_schemac::print_program(&catalog, &program).into_bytes())
        })(),
        _ => return None,
    };
    Some(output)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(output) = run(&args) else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
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
