//! Offline compliance operator binary.

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::io::IsTerminal;
use std::process::ExitCode;

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn requires_private_stdin(args: &[OsString]) -> bool {
    args.first()
        .is_some_and(|command| command == OsStr::new("unseal") || command == OsStr::new("hold"))
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();

    // The library returns UnsupportedPlatform before environment/config/path access. Do not even
    // acquire or inspect the real stdin handle on targets where offline custody is not shipped.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let mut stdin = io::empty();
        let mut stdout = io::stdout().lock();
        finish(pigeonpost_compliance::run_operator_cli(
            args,
            &mut stdin,
            &mut stdout,
        ))
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let stdin = io::stdin();
        if requires_private_stdin(&args) && stdin.is_terminal() {
            eprintln!(
            "ppcompliance: private request stdin must come from the protected case-management boundary"
        );
            return ExitCode::FAILURE;
        }
        let mut stdin = stdin.lock();
        let mut stdout = io::stdout().lock();
        finish(pigeonpost_compliance::run_operator_cli(
            args,
            &mut stdin,
            &mut stdout,
        ))
    }
}

fn finish(result: Result<(), pigeonpost_compliance::OperatorError>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ppcompliance: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_case_commands_require_noninteractive_private_stdin() {
        assert!(requires_private_stdin(&[OsString::from("unseal")]));
        assert!(requires_private_stdin(&[OsString::from("hold")]));
        assert!(!requires_private_stdin(&[OsString::from("status")]));
        assert!(!requires_private_stdin(&[OsString::from("--version")]));
    }
}
