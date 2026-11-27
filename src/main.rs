use clap::Parser;
use jired::cli::{self, Cli};

fn main() -> std::process::ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // In `--json` mode a bad flag is a `jired/error/1` like any other
        // usage error: clap's prose on stderr was all an agent got. Help and
        // the version are what was asked for, and print as clap prints them.
        Err(e) if wants_json() && e.use_stderr() => {
            let text = e.render().to_string();
            let message = text
                .lines()
                .next()
                .unwrap_or_default()
                .trim_start_matches("error: ")
                .to_string();
            let err = jired::JiredError::usage(message)
                .with_detail("clap", format!("{:?}", e.kind()))
                .with_hint("`jired help` lists the commands, `jired <command> --help` their flags");
            eprintln!("{}", err.to_json());
            return std::process::ExitCode::from(err.exit_code().as_i32() as u8);
        }
        Err(e) => e.exit(),
    };
    let code = cli::run(cli);
    std::process::ExitCode::from(code.as_i32() as u8)
}

/// `--json` among the arguments, for an error clap raises before there is a
/// parsed flag to ask.
fn wants_json() -> bool {
    std::env::args_os().skip(1).any(|a| a == "--json")
}
