use clap::Parser;
use jired::cli::{self, Cli};

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let code = cli::run(cli);
    std::process::ExitCode::from(code.as_i32() as u8)
}
