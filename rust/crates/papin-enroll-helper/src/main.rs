//! Thin setuid-root shell around the validated `apply` action (§7.1). All
//! security decisions live in the (unit-tested) library; this file only
//! performs the root-only step: absolute, shell-free `wg syncconf`.

use papin_enroll_helper::{
    parse_args, validate_path, validate_peers, wg_command, Action, PEERS_DIR,
};
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Ok(Action::Apply(path)) = parse_args(&args) else {
        eprintln!("error: usage: papin-enroll-helper apply <peers-file>");
        return ExitCode::from(2);
    };

    // Defense in depth behind the gateway's bearer checks: the path must be a
    // regular file inside /run/papin/wg-peers/ with strict contents.
    let canonical = match validate_path(&path, Path::new(PEERS_DIR)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: invalid peers file: {e}");
            return ExitCode::from(2);
        }
    };
    let contents = match std::fs::read_to_string(&canonical) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read peers file: {e}");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = validate_peers(&contents) {
        eprintln!("error: invalid peers file contents: {e}");
        return ExitCode::from(2);
    }

    // The only privileged action in the entire system. No shell, absolute
    // execve, scrubbed environment (built in wg_command).
    let mut child = match wg_command(&canonical).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "error: cannot exec {WG}: {e}",
                WG = papin_enroll_helper::WG_BIN
            );
            return ExitCode::from(1);
        }
    };
    // Propagate wg's exit status; capture stderr for the gateway's log.
    match child.wait() {
        Ok(status) if status.success() => {
            println!("ok");
            ExitCode::SUCCESS
        }
        Ok(status) => {
            let mut err = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut err);
            }
            eprintln!("error: wg syncconf failed ({status}): {err}");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("error: wait failed: {e}");
            ExitCode::from(1)
        }
    }
}
