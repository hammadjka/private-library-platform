use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    process::{Child, ChildStderr, Command, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct RunningServer {
    child: Child,
    address: SocketAddr,
    media_path: PathBuf,
    stderr_thread: Option<JoinHandle<()>>,
}

struct HttpResponse {
    status_line: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(stderr_thread) = self.stderr_thread.take() {
            let _ = stderr_thread.join();
        }
        let _ = fs::remove_file(&self.media_path);
    }
}

fn start_server(bytes: &[u8]) -> RunningServer {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after Unix epoch")
        .as_nanos();
    let media_path = std::env::temp_dir().join(format!("plpd-media-{unique}.mp4"));
    fs::write(&media_path, bytes).expect("write test media");

    let mut child = Command::new(env!("CARGO_BIN_EXE_plpd"))
        .args([
            "--media",
            media_path.to_str().expect("media path is UTF-8"),
            "--bind",
            "127.0.0.1:0",
        ])
        .stderr(Stdio::piped())
        .spawn()
        .expect("start daemon");

    let stderr = child.stderr.take().expect("capture daemon stderr");
    let (startup_sender, startup_receiver) = mpsc::sync_channel(1);
    let stderr_thread = thread::spawn(move || drain_server_stderr(stderr, startup_sender));

    let address = match startup_receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(address) => address,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_thread.join();
            let _ = fs::remove_file(&media_path);
            panic!("daemon did not report its listening address: {error}");
        }
    };

    RunningServer {
        child,
        address,
        media_path,
        stderr_thread: Some(stderr_thread),
    }
}

fn drain_server_stderr(stderr: ChildStderr, startup_sender: mpsc::SyncSender<SocketAddr>) {
    let mut startup_sender = Some(startup_sender);

    for line in BufReader::new(stderr).lines() {
        match line {
            Ok(line) => {
                if let (Some(address), Some(sender)) =
                    (startup_address(&line), startup_sender.take())
                {
                    let _ = sender.send(address);
                }
                eprintln!("{line}");
            }
            Err(error) => {
                eprintln!("failed to read daemon stderr: {error}");
                break;
            }
        }
    }
}

fn startup_address(line: &str) -> Option<SocketAddr> {
    let (_, address) = line.rsplit_once(" at http://")?;
    address.strip_suffix("/media")?.parse().ok()
}

fn request(address: SocketAddr, request: &str) -> HttpResponse {
    let mut stream = TcpStream::connect(address).expect("connect to daemon");
    stream
        .write_all(request.as_bytes())
        .expect("send HTTP request");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    parse_response(response)
}

fn parse_response(response: Vec<u8>) -> HttpResponse {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response contains a header terminator");
    let head = std::str::from_utf8(&response[..header_end]).expect("response headers are UTF-8");
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("response has a status line").to_owned();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').expect("response header contains ':'");
            (name.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();

    HttpResponse {
        status_line,
        headers,
        body: response[header_end + 4..].to_vec(),
    }
}

#[test]
fn serves_a_browser_player_at_the_root_url() {
    let server = start_server(b"video-bytes");

    let response = request(
        server.address,
        "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(response.status_line, "HTTP/1.1 200 OK");
    assert_eq!(
        response.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    let body = String::from_utf8(response.body).expect("player page is UTF-8");

    assert!(body.contains("<video"));
    assert!(body.contains("src=\"/media\""));
    assert!(body.contains("controls"));
}

#[test]
fn streams_the_entire_media_file() {
    let server = start_server(b"video-bytes");

    let response = request(
        server.address,
        "GET /media HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );

    assert_eq!(response.status_line, "HTTP/1.1 200 OK");
    assert_eq!(response.header("content-length"), Some("11"));
    assert_eq!(response.header("content-type"), Some("video/mp4"));
    assert_eq!(response.header("accept-ranges"), Some("bytes"));
    assert_eq!(response.body, b"video-bytes");
}

#[test]
fn streams_supported_single_byte_ranges() {
    let server = start_server(b"video-bytes");

    let cases = [
        ("bytes=2-5", "bytes 2-5/11", "deo-"),
        ("bytes=6-", "bytes 6-10/11", "bytes"),
        ("bytes=-5", "bytes 6-10/11", "bytes"),
        ("bytes=6-999", "bytes 6-10/11", "bytes"),
        ("bytes=-999", "bytes 0-10/11", "video-bytes"),
        ("Bytes=6-10", "bytes 6-10/11", "bytes"),
    ];

    for (range, content_range, body) in cases {
        let response = request(
            server.address,
            &format!(
                "GET /media HTTP/1.1\r\nHost: localhost\r\nRange: {range}\r\nConnection: close\r\n\r\n"
            ),
        );
        assert_eq!(
            response.status_line, "HTTP/1.1 206 Partial Content",
            "range {range}"
        );
        assert_eq!(
            response.header("content-length"),
            Some(body.len().to_string().as_str()),
            "range {range}"
        );
        assert_eq!(
            response.header("content-range"),
            Some(content_range),
            "range {range}"
        );
        assert_eq!(response.header("content-type"), Some("video/mp4"));
        assert_eq!(response.header("accept-ranges"), Some("bytes"));
        assert_eq!(response.body, body.as_bytes(), "range {range}");
    }
}

#[test]
fn rejects_invalid_unsatisfiable_or_multiple_byte_ranges() {
    let server = start_server(b"video-bytes");
    let ranges = [
        "bytes=8-2",
        "bytes=not-a-range",
        "bytes=1-2-3",
        "bytes=-",
        "bytes=11-12",
        "bytes=-0",
        "bytes=18446744073709551616-",
        "bytes=0-1,4-5",
    ];

    for range in ranges {
        let response = request(
            server.address,
            &format!(
                "GET /media HTTP/1.1\r\nHost: localhost\r\nRange: {range}\r\nConnection: close\r\n\r\n"
            ),
        );

        assert_eq!(
            response.status_line, "HTTP/1.1 416 Range Not Satisfiable",
            "range {range}"
        );
        assert_eq!(response.header("content-length"), Some("0"));
        assert_eq!(response.header("content-range"), Some("bytes */11"));
        assert_eq!(response.header("accept-ranges"), Some("bytes"));
        assert!(response.body.is_empty());
    }
}

#[test]
fn ignores_an_unsupported_range_unit() {
    let server = start_server(b"video-bytes");

    let response = request(
        server.address,
        "GET /media HTTP/1.1\r\nHost: localhost\r\nRange: items=0-1\r\nConnection: close\r\n\r\n",
    );

    assert_eq!(response.status_line, "HTTP/1.1 200 OK");
    assert_eq!(response.header("content-length"), Some("11"));
    assert_eq!(response.header("content-type"), Some("video/mp4"));
    assert_eq!(response.header("accept-ranges"), Some("bytes"));
    assert_eq!(response.body, b"video-bytes");
}

#[test]
fn handles_an_empty_media_file() {
    let server = start_server(b"");

    let full_response = request(
        server.address,
        "GET /media HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(full_response.status_line, "HTTP/1.1 200 OK");
    assert_eq!(full_response.header("content-length"), Some("0"));
    assert_eq!(full_response.header("content-type"), Some("video/mp4"));
    assert_eq!(full_response.header("accept-ranges"), Some("bytes"));
    assert!(full_response.body.is_empty());

    let range_response = request(
        server.address,
        "GET /media HTTP/1.1\r\nHost: localhost\r\nRange: bytes=0-0\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(
        range_response.status_line,
        "HTTP/1.1 416 Range Not Satisfiable"
    );
    assert_eq!(range_response.header("content-length"), Some("0"));
    assert_eq!(range_response.header("content-range"), Some("bytes */0"));
    assert_eq!(range_response.header("accept-ranges"), Some("bytes"));
    assert!(range_response.body.is_empty());
}

#[test]
fn head_describes_media_without_sending_its_bytes() {
    let server = start_server(b"video-bytes");

    let response = request(
        server.address,
        "HEAD /media HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );

    assert_eq!(response.status_line, "HTTP/1.1 200 OK");
    assert_eq!(response.header("content-length"), Some("11"));
    assert_eq!(response.header("content-type"), Some("video/mp4"));
    assert_eq!(response.header("accept-ranges"), Some("bytes"));
    assert!(response.body.is_empty());
}

#[test]
fn head_ignores_ranges_and_sends_no_media_bytes() {
    let server = start_server(b"video-bytes");
    let ranges = ["bytes=6-", "bytes=0-1,4-5", "bytes=not-a-range"];

    for range in ranges {
        let response = request(
            server.address,
            &format!(
                "HEAD /media HTTP/1.1\r\nHost: localhost\r\nRange: {range}\r\nConnection: close\r\n\r\n"
            ),
        );
        assert_eq!(response.status_line, "HTTP/1.1 200 OK", "range {range}");
        assert_eq!(response.header("content-length"), Some("11"));
        assert_eq!(response.header("content-type"), Some("video/mp4"));
        assert_eq!(response.header("accept-ranges"), Some("bytes"));
        assert!(response.header("content-range").is_none());
        assert!(response.body.is_empty(), "range {range}");
    }
}
