use std::{env, path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).map(PathBuf::from).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("Usage: fixture-dbc2dbf INPUT.dbc OUTPUT.dbf");
        return ExitCode::FAILURE;
    };
    if let Err(error) = datasus_dbc::decompress(input, output) {
        eprintln!("DBC decompression failed for {}: {error:?}", input.display());
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
