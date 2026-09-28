//! Thin shell around the (unit-tested) wrapper sequence. fd 0 = the
//! socket-activated bootstrap stream, fd 1 = the handshake line back to the
//! gateway; stderr goes to the journal (StandardError=journal in the unit).

use papin_acp_enter::{Reason, System, READY_MARKER};
use std::io::{BufRead, Write};
use std::path::Path;
use std::process::ExitCode;

struct RealSystem {
    stdin: std::io::BufReader<std::io::Stdin>,
}

impl RealSystem {
    fn new() -> RealSystem {
        RealSystem {
            stdin: std::io::BufReader::new(std::io::stdin()),
        }
    }
}

impl System for RealSystem {
    fn read_bootstrap_line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.stdin.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    }

    fn write_line(&mut self, line: &str) {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = out.write_all(line.as_bytes());
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }

    fn marker_exists(&self, root: &Path) -> bool {
        root.join(READY_MARKER).is_file()
    }

    fn read_env_file(&self, agents: &Path, id: &str) -> Vec<(String, String)> {
        std::fs::read_to_string(agents.join(format!("{id}.env")))
            .map(|c| papin_acp_enter::parse_env(&c))
            .unwrap_or_default()
    }

    fn chroot(&self, root: &Path) -> Result<(), Reason> {
        let c_path = std::ffi::CString::new(root.as_os_str().as_encoded_bytes())
            .map_err(|_| Reason::RootfsNotReady)?;
        // Ambient CAP_SYS_CHROOT (granted by the unit) makes this succeed as
        // the unprivileged DynamicUser. No CAP_SYS_ADMIN anywhere.
        let rc = unsafe { libc::chroot(c_path.as_ptr()) };
        if rc != 0 {
            return Err(Reason::RootfsNotReady);
        }
        let slash = std::ffi::CString::new("/").unwrap();
        let rc = unsafe { libc::chdir(slash.as_ptr()) };
        if rc != 0 {
            return Err(Reason::RootfsNotReady);
        }
        Ok(())
    }

    fn exec(&self, path: &Path, env: &[(String, String)]) -> Result<(), Reason> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| Reason::ExecFailed)?;
        let c_arg0 = CString::new("papin-acp").map_err(|_| Reason::ExecFailed)?;
        let args: Vec<CString> = vec![c_arg0];
        let mut envp: Vec<CString> = env
            .iter()
            .map(|(k, v)| CString::new(format!("{k}={v}")))
            .collect::<Result<_, _>>()
            .map_err(|_| Reason::ExecFailed)?;
        envp.shrink_to_fit();
        let mut arg_ptrs: Vec<*const libc::c_char> = args.iter().map(|s| s.as_ptr()).collect();
        arg_ptrs.push(std::ptr::null());
        let mut env_ptrs: Vec<*const libc::c_char> = envp.iter().map(|s| s.as_ptr()).collect();
        env_ptrs.push(std::ptr::null());
        // NoNewPrivileges=yes (unit) clears the ambient capability set on
        // execve: post-exec the agent is a plain unprivileged process in the
        // chroot with the two-cap bounding set.
        let rc = unsafe { libc::execve(c_path.as_ptr(), arg_ptrs.as_ptr(), env_ptrs.as_ptr()) };
        // Only reached if execve failed.
        let _ = rc;
        Err(Reason::ExecFailed)
    }
}

fn main() -> ExitCode {
    let mut sys = RealSystem::new();
    match papin_acp_enter::run(&mut sys) {
        Ok(()) => ExitCode::SUCCESS, // unreachable for a real execve
        Err(_) => ExitCode::from(1),
    }
}
