//! JSON command-line front end: reads the input document from stdin and
//! writes the consensus report to stdout. Errors go to stderr as JSON with
//! a non-zero exit code.

use std::io::Read;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut buffer = String::new();
    if let Err(source) = std::io::stdin().read_to_string(&mut buffer) {
        return fail(&format!("failed to read stdin: {source}"));
    }
    match clades::run(&buffer) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("{}", serde_json::json!({ "error": message }));
    ExitCode::FAILURE
}
