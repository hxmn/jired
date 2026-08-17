//! `jired completion <shell>` [M5.3].

use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::{Args, CommandFactory};
use clap_complete::Shell;

#[derive(Args, Debug)]
pub struct CompletionArgs {
    /// Shell to generate a completion script for
    #[arg(value_enum)]
    pub shell: Shell,
}

pub fn run(args: &CompletionArgs, _out: Out) -> Result<ExitCode> {
    let mut cmd = crate::cli::Cli::command();
    let name = cmd.get_name().to_string();
    clap_complete::generate(args.shell, &mut cmd, name, &mut std::io::stdout());
    Ok(ExitCode::Success)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_for_every_supported_shell() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell] {
            let mut cmd = crate::cli::Cli::command();
            let mut buf: Vec<u8> = Vec::new();
            clap_complete::generate(shell, &mut cmd, "jired", &mut buf);
            assert!(!buf.is_empty(), "{shell:?} produced no script");
        }
    }
}
