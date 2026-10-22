//! Owned server processes and a bounded HTTP oracle for CLI integration tests.

use process_wrap::std::{ChildWrapper, CommandWrap};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Keeps diagnostics on disk so a server cannot block on a full output pipe.
pub struct Server {
    child: Box<dyn ChildWrapper>,
    stdout: tempfile::NamedTempFile,
    stderr: tempfile::NamedTempFile,
    stopped: bool,
}

impl Server {
    fn parent_status(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        // Poll only the leader. JobObject::try_wait drains completion messages
        // that stop() must retain when it waits for the entire owned tree.
        self.child.inner_mut().try_wait()
    }

    pub fn spawn(mut command: Command) -> io::Result<Self> {
        let stdout = tempfile::NamedTempFile::new()?;
        let stderr = tempfile::NamedTempFile::new()?;
        command
            .stdin(Stdio::null())
            .stdout(stdout.reopen()?)
            .stderr(stderr.reopen()?);
        let mut command = CommandWrap::from(command);
        #[cfg(unix)]
        command.wrap(process_wrap::std::ProcessGroup::leader());
        #[cfg(windows)]
        command.wrap(process_wrap::std::JobObject);
        Ok(Self {
            child: command.spawn()?,
            stdout,
            stderr,
            stopped: false,
        })
    }

    /// Retry pending connections until the deadline; a response must be correct.
    pub fn wait_for_response(
        &mut self,
        address: SocketAddr,
        expected_body: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.parent_status().map_err(|e| e.to_string())? {
                return Err(format!("server exited before readiness: {status}"));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("server readiness deadline expired".into());
            }
            match TcpStream::connect_timeout(&address, remaining.min(Duration::from_millis(100))) {
                Ok(mut stream) => {
                    stream
                        .set_write_timeout(Some(remaining.min(Duration::from_secs(1))))
                        .map_err(|e| e.to_string())?;
                    stream
                        .write_all(
                            b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                        )
                        .map_err(|e| format!("HTTP request failed: {e}"))?;
                    let mut response = Vec::new();
                    loop {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            return Err("HTTP response deadline expired".into());
                        }
                        stream
                            .set_read_timeout(Some(remaining.min(Duration::from_secs(1))))
                            .map_err(|e| e.to_string())?;
                        let mut chunk = [0; 1024];
                        let count = stream
                            .read(&mut chunk)
                            .map_err(|e| format!("HTTP response failed: {e}"))?;
                        if count == 0 {
                            break;
                        }
                        response.extend_from_slice(&chunk[..count]);
                        if response.len() > 65536 {
                            return Err("HTTP response exceeds limit".into());
                        }
                    }
                    let response = String::from_utf8(response).map_err(|e| e.to_string())?;
                    let (headers, body) = response
                        .split_once("\r\n\r\n")
                        .ok_or_else(|| format!("invalid HTTP response: {response:?}"))?;
                    let mut status = headers
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .split_whitespace();
                    let version = status.next();
                    if !matches!(version, Some("HTTP/1.0" | "HTTP/1.1"))
                        || status.next() != Some("404")
                        || body != expected_body
                    {
                        return Err(format!("unexpected HTTP response: {response:?}"));
                    }
                    if self.parent_status().map_err(|e| e.to_string())?.is_some() {
                        return Err("server exited while responding".into());
                    }
                    return Ok(());
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(format!("HTTP connect failed: {error}")),
            }
            std::thread::sleep(Duration::from_millis(20).min(remaining));
        }
    }

    pub fn diagnostics(&self) -> String {
        format!(
            "stdout:\n{}\nstderr:\n{}",
            std::fs::read_to_string(self.stdout.path()).unwrap_or_default(),
            std::fs::read_to_string(self.stderr.path()).unwrap_or_default()
        )
    }

    /// Observe an exact program output line and its parent's readiness marker.
    /// Runtime children share stderr and may leave an unterminated prefix.
    #[cfg(all(feature = "beam-runtime", feature = "cli-watch"))]
    pub fn wait_for_output(
        &mut self,
        stdout_line: &str,
        stderr_marker: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.parent_status().map_err(|e| e.to_string())? {
                return Err(format!("process exited before expected output: {status}"));
            }
            let stdout = std::fs::read_to_string(self.stdout.path()).map_err(|e| e.to_string())?;
            let stderr = std::fs::read_to_string(self.stderr.path()).map_err(|e| e.to_string())?;
            if stdout.lines().any(|line| line == stdout_line) && stderr.contains(stderr_marker) {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "expected output deadline expired: {stdout_line:?}, {stderr_marker:?}"
                ));
            }
            std::thread::sleep(Duration::from_millis(20).min(remaining));
        }
    }

    /// Kill the owned group/job even if its leader has already exited.
    pub fn stop(&mut self) -> io::Result<()> {
        if self.stopped {
            return Ok(());
        }
        let killed = self.child.start_kill();
        let waited = self.child.wait();
        waited?;
        if let Err(error) = killed {
            // A reaped leader with no remaining descendants has no Unix group.
            #[cfg(unix)]
            let group_gone = error.raw_os_error() == Some(libc::ESRCH);
            #[cfg(not(unix))]
            let group_gone = false;
            if !group_gone
                && !matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
                )
            {
                return Err(error);
            }
        }
        self.stopped = true;
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("server cleanup failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn address() -> SocketAddr {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
    }

    fn fixture(mode: &str, address: SocketAddr) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "server::tests::fixture_process", "--nocapture"])
            .env("RIDGE_CLI_SERVER_FIXTURE", mode)
            .env("RIDGE_CLI_SERVER_ADDRESS", address.to_string());
        command
    }

    fn released(address: SocketAddr) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if TcpListener::bind(address).is_ok() {
                return;
            }
            assert!(Instant::now() < deadline, "listener leaked at {address}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A subprocess fixture, a no-op in an ordinary suite invocation.
    #[test]
    fn fixture_process() {
        let Ok(mode) = std::env::var("RIDGE_CLI_SERVER_FIXTURE") else {
            return;
        };
        let address: SocketAddr = std::env::var("RIDGE_CLI_SERVER_ADDRESS")
            .unwrap()
            .parse()
            .unwrap();
        match mode.as_str() {
            "output" | "prefixed-output" => {
                println!("owned output");
                if mode == "prefixed-output" {
                    eprint!("Logger - error: ");
                    io::stderr().flush().unwrap();
                }
                eprintln!("owned readiness");
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            "exit" => {
                eprintln!("fixture startup failed");
                std::process::exit(7);
            }
            "tree" => {
                let mut child = fixture("serve", address).spawn().unwrap();
                child.wait().unwrap();
            }
            "serve" | "wrong" => {
                let listener = TcpListener::bind(address).unwrap();
                for connection in listener.incoming() {
                    let mut connection = connection.unwrap();
                    connection
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut request = [0; 1024];
                    let _ = connection.read(&mut request);
                    let response = if mode == "serve" {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 5\r\n\r\nowned"
                    } else {
                        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nwrong"
                    };
                    let _ = connection.write_all(response.as_bytes());
                }
            }
            "hang" => loop {
                std::thread::sleep(Duration::from_secs(1));
            },
            _ => panic!("unknown fixture mode"),
        }
    }

    #[test]
    fn startup_failure_is_rejected_with_diagnostics() {
        let address = address();
        let mut server = Server::spawn(fixture("exit", address)).unwrap();
        let result = server.wait_for_response(address, "owned", Duration::from_secs(5));
        assert!(result.unwrap_err().contains("exited before readiness"));
        assert!(server.diagnostics().contains("fixture startup failed"));
        server.stop().unwrap();
        released(address);
    }

    #[test]
    fn wrong_response_is_rejected_and_cleaned_up() {
        let address = address();
        let mut server = Server::spawn(fixture("wrong", address)).unwrap();
        assert!(server
            .wait_for_response(address, "owned", Duration::from_secs(5))
            .unwrap_err()
            .contains("unexpected HTTP response"));
        server.stop().unwrap();
        released(address);
    }

    #[test]
    fn readiness_has_a_deadline() {
        let address = address();
        let mut server = Server::spawn(fixture("hang", address)).unwrap();
        assert!(server
            .wait_for_response(address, "owned", Duration::from_millis(150))
            .unwrap_err()
            .contains("readiness deadline"));
        server.stop().unwrap();
    }

    #[cfg(all(feature = "beam-runtime", feature = "cli-watch"))]
    #[test]
    fn output_readiness_survives_an_unterminated_runtime_message() {
        let mut server = Server::spawn(fixture("prefixed-output", address())).unwrap();
        let result =
            server.wait_for_output("owned output", "owned readiness", Duration::from_secs(5));
        let diagnostics = server.diagnostics();
        server.stop().unwrap();
        assert!(result.is_ok(), "{result:?}\n{diagnostics}");
    }

    #[cfg(all(feature = "beam-runtime", feature = "cli-watch"))]
    #[test]
    fn output_readiness_requires_the_program_marker_and_a_live_parent() {
        let address = address();
        let mut server = Server::spawn(fixture("output", address)).unwrap();
        server
            .wait_for_output("owned output", "owned readiness", Duration::from_secs(5))
            .unwrap();
        assert!(server
            .wait_for_output(
                "owned output",
                "missing readiness",
                Duration::from_millis(150)
            )
            .unwrap_err()
            .contains("deadline expired"));
        assert!(server
            .wait_for_output(
                "edited output",
                "owned readiness",
                Duration::from_millis(150)
            )
            .unwrap_err()
            .contains("deadline expired"));
        server.stop().unwrap();
        let mut exited = Server::spawn(fixture("exit", address)).unwrap();
        assert!(exited
            .wait_for_output("owned output", "owned readiness", Duration::from_secs(5))
            .unwrap_err()
            .contains("exited before expected output"));
        exited.stop().unwrap();
    }

    #[test]
    fn unwinding_cleans_up_the_descendant_listener() {
        let address = address();
        let mut server = Server::spawn(fixture("tree", address)).unwrap();
        server
            .wait_for_response(address, "owned", Duration::from_secs(5))
            .unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _server = server;
            panic!("exercise cleanup during an assertion failure");
        }));
        assert!(result.is_err());
        released(address);
    }

    #[test]
    fn another_servers_response_cannot_satisfy_readiness() {
        let address = address();
        let mut other = Server::spawn(fixture("serve", address)).unwrap();
        other
            .wait_for_response(address, "owned", Duration::from_secs(5))
            .unwrap();
        let mut server = Server::spawn(fixture("hang", address)).unwrap();
        assert!(server
            .wait_for_response(address, "different instance", Duration::from_secs(5))
            .is_err());
        server.stop().unwrap();
        other
            .wait_for_response(address, "owned", Duration::from_secs(5))
            .unwrap();
        other.stop().unwrap();
        released(address);
    }
}
