//! Real-socket checks for complete, unambiguous HTTP request bodies.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_codegen_erl::{erlc, output_layout, runtime};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Server {
    child: Child,
    port: u16,
    _directory: tempfile::TempDir,
}

impl Server {
    fn start() -> Self {
        let directory = tempfile::tempdir().expect("runtime directory");
        output_layout::ensure_out_dirs(directory.path()).unwrap();
        runtime::install_runtime(directory.path()).unwrap();
        let compiler = erlc::probe(None).expect("beam-runtime requires OTP");
        runtime::compile_runtime(&compiler.path, directory.path()).unwrap();
        let eval = r#"
            ets:new(http_hits, [named_table, public]),
            ets:insert(http_hits, {hits, 0}),
            Handler = fun(#{path := Path, body := Body}) ->
                case Path of
                    <<"/count">> ->
                        [{hits, N}] = ets:lookup(http_hits, hits),
                        #{status => 200, body => integer_to_binary(N)};
                    _ ->
                        ets:update_counter(http_hits, hits, 1),
                        #{status => 200, body => Body}
                end
            end,
            spawn(fun() -> ridge_rt:http_listen(0, Handler) end),
            Wait = fun F() ->
                case ridge_rt:http_port() of
                    Port when is_integer(Port) -> Port;
                    _ -> timer:sleep(10), F()
                end
            end,
            io:format("~p~n", [Wait()]),
            receive stop -> ok after 60000 -> ok end,
            halt().
        "#;
        let child = Command::new(which::which("erl").expect("beam-runtime requires erl"))
            .args(["-noshell", "-pa"])
            .arg(output_layout::beam_dir(directory.path()))
            .args(["-eval", eval])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut server = Self {
            child,
            port: 0,
            _directory: directory,
        };
        let stdout = server.child.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout).read_line(&mut line);
            let _ = sender.send((result, line));
        });
        let (result, line) = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("HTTP server startup deadline");
        result.expect("HTTP server startup output");
        server.port = line.trim().parse().expect("bound HTTP port");
        server
    }

    fn connect(&self) -> TcpStream {
        let stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(7)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(7)))
            .unwrap();
        stream
    }

    fn request(&self, chunks: &[&[u8]]) -> (u16, Vec<u8>) {
        let mut stream = self.connect();
        for (i, chunk) in chunks.iter().enumerate() {
            stream.write_all(chunk).unwrap();
            if i + 1 < chunks.len() {
                stream
                    .set_read_timeout(Some(Duration::from_millis(80)))
                    .unwrap();
                let mut byte = [0];
                let error = stream
                    .read(&mut byte)
                    .expect_err("response before complete request");
                assert!(
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ),
                    "unexpected premature socket failure: {error}"
                );
                stream
                    .set_read_timeout(Some(Duration::from_secs(7)))
                    .unwrap();
            }
        }
        response(&mut stream)
    }

    fn hits(&self) -> u64 {
        let (status, body) = self.request(&[b"GET /count HTTP/1.1\r\nHost: localhost\r\n\r\n"]);
        assert_eq!(status, 200);
        std::str::from_utf8(&body).unwrap().parse().unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn response(stream: &mut TcpStream) -> (u16, Vec<u8>) {
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).expect("complete HTTP response");
        assert_ne!(
            count, 0,
            "connection closed before complete response: {bytes:?}"
        );
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(split) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            let header = std::str::from_utf8(&bytes[..split]).unwrap();
            let length: usize = header
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .unwrap()
                .parse()
                .unwrap();
            if bytes.len() >= split + 4 + length {
                assert_eq!(bytes.len(), split + 4 + length, "extra response bytes");
                break;
            }
        }
    }
    let split = bytes
        .windows(4)
        .position(|b| b == b"\r\n\r\n")
        .expect("response headers");
    let header = std::str::from_utf8(&bytes[..split]).unwrap();
    let status = header.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, bytes[split + 4..].to_vec())
}

#[test]
fn http_dispatch_requires_complete_framed_body() {
    let server = Server::start();
    assert_complete_bodies(&server);
    assert_rejected_framing(&server);
    assert_receive_limits(&server);
}

fn assert_complete_bodies(server: &Server) {
    let headers = b"POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\n\r\n";
    let joined = [headers.as_slice(), b"alpha"].concat();
    for chunks in [
        vec![joined.as_slice()],
        vec![headers.as_slice(), b"alpha"],
        vec![&joined[..joined.len() - 3], b"pha"],
        vec![&headers[..headers.len() - 1], b"\n", b"alpha"],
        vec![&headers[..headers.len() - 2], b"\r\n", b"alpha"],
        vec![&headers[..headers.len() - 3], b"\n\r\n", b"alpha"],
    ] {
        let (status, body) = server.request(&chunks);
        assert_eq!(status, 200);
        assert_eq!(body, b"alpha");
    }
    let utf8 = "niña\r\n🦀".as_bytes();
    let utf8_headers = format!(
        "POST /echo HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
        utf8.len()
    );
    let (status, body) = server.request(&[utf8_headers.as_bytes(), &utf8[..3], &utf8[3..]]);
    assert_eq!(status, 200);
    assert_eq!(body, utf8);
    for (name, request, expected) in [
        ("no length", "GET /echo HTTP/1.1\r\n\r\n", ""),
        (
            "zero length",
            "POST /echo HTTP/1.1\r\nContent-Length: 0\r\n\r\n",
            "",
        ),
        ("unframed suffix", "POST /echo HTTP/1.1\r\n\r\nextra", ""),
        (
            "extra bytes",
            "POST /echo HTTP/1.1\r\nContent-Length: 5\r\n\r\nalphaextra",
            "alpha",
        ),
        (
            "case and OWS",
            "POST /echo HTTP/1.0\r\ncOnTeNt-LeNgTh:\t 5 \t\r\n\r\nalpha",
            "alpha",
        ),
        (
            "identical lengths",
            "POST /echo HTTP/1.1\r\nContent-Length: 005, 5\r\nContent-Length: 5\r\n\r\nalpha",
            "alpha",
        ),
    ] {
        eprintln!("valid framing: {name}");
        let (status, body) = server.request(&[request.as_bytes()]);
        assert_eq!(status, 200, "{name}");
        assert_eq!(body, expected.as_bytes(), "{name}");
    }
}

fn assert_rejected_framing(server: &Server) {
    let before = server.hits();
    for (name, fields, expected_status) in [
        ("negative", "Content-Length: -1", 400),
        ("signed", "Content-Length: +5", 400),
        ("nondecimal", "Content-Length: 5x", 400),
        ("empty", "Content-Length:", 400),
        ("conflict", "Content-Length: 5\r\nContent-Length: 6", 400),
        ("comma conflict", "Content-Length: 5,6", 400),
        ("oversized", "Content-Length: 8388609", 413),
        (
            "huge integer",
            "Content-Length: 99999999999999999999999999",
            413,
        ),
        ("transfer coding", "Transfer-Encoding: chunked", 501),
        (
            "ambiguous framing",
            "Content-Length: 5\r\nTransfer-Encoding: chunked",
            400,
        ),
        ("invalid header", "Content Length: 5", 400),
        ("bare LF", "X: v\nY: v", 400),
        ("bare CR", "X: v\rY: v", 400),
    ] {
        eprintln!("rejected framing: {name}");
        let request = format!("POST /echo HTTP/1.1\r\n{fields}\r\n\r\n");
        let (status, _) = server.request(&[request.as_bytes()]);
        assert_eq!(status, expected_status, "{name}");
        assert_eq!(server.hits(), before, "{name} reached handler");
    }
}

fn assert_receive_limits(server: &Server) {
    let before = server.hits();
    let headers = b"POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\n\r\n";
    let oversized_line = format!(
        "GET /echo HTTP/1.1\r\nX-Fill: {}\r\n\r\n",
        "x".repeat(65536)
    );
    assert_eq!(server.request(&[oversized_line.as_bytes()]).0, 431);
    assert_eq!(server.hits(), before);

    let prefix = "GET /echo HTTP/1.1\r\nX-Fill: ";
    let boundary = format!("{prefix}{}\r\n\r\n", "x".repeat(65536 - prefix.len() - 4));
    assert_eq!(server.request(&[boundary.as_bytes()]), (200, Vec::new()));
    let too_many_headers = boundary.replace("\r\n\r\n", "\r\nX: y\r\n\r\n");
    let before = server.hits();
    assert_eq!(server.request(&[too_many_headers.as_bytes()]).0, 431);
    assert_eq!(server.hits(), before);

    let maximum_body = vec![b'x'; 8 * 1024 * 1024];
    let maximum_headers = b"POST /echo HTTP/1.1\r\nContent-Length: 8388608\r\n\r\n";
    let (status, body) = server.request(&[maximum_headers.as_slice(), &maximum_body]);
    assert_eq!(status, 200);
    assert_eq!(body, maximum_body);
    let before = server.hits();

    let mut truncated = server.connect();
    truncated.write_all(headers).unwrap();
    truncated.write_all(b"al").unwrap();
    truncated.shutdown(Shutdown::Write).unwrap();
    let mut closed = Vec::new();
    truncated.read_to_end(&mut closed).unwrap();
    assert!(
        closed.is_empty(),
        "EOF must not become a successful request"
    );
    assert_eq!(server.hits(), before);

    let mut timed_out = server.connect();
    let start = std::time::Instant::now();
    timed_out.write_all(b"POST /echo HTTP/1.1\r\n").unwrap();
    std::thread::sleep(Duration::from_secs(4));
    timed_out.write_all(b"Content-Length: 5\r\n\r\nal").unwrap();
    assert_eq!(response(&mut timed_out).0, 408);
    assert!(
        start.elapsed() < Duration::from_millis(6500),
        "deadline restarted after headers"
    );
    assert_eq!(server.hits(), before);
}
