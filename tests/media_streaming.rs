use std::{
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

fn request(address: SocketAddr, request: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(address).expect("connect to daemon");
    stream
        .write_all(request.as_bytes())
        .expect("send HTTP request");
    stream
        .shutdown(std::net::Shutdown::Write)
        .expect("finish request");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

#[test]
fn serves_a_browser_player_at_the_root_url() {
    let server = start_server(b"video-bytes");

    let response = request(
        server.address,
        "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    let response = String::from_utf8(response).expect("player page is UTF-8");

    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.contains("Content-Type: text/html; charset=utf-8\r\n"));
    assert!(response.contains("<video"));
    assert!(response.contains("src=\"/media\""));
    assert!(response.contains("controls"));
}

#[test]
fn streams_the_entire_media_file() {
    let server = start_server(b"video-bytes");

    let response = request(
        server.address,
        "GET /media HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );

    assert_eq!(
        response,
        b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\nvideo-bytes"
    );
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
        let expected = format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: {content_range}\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );

        assert_eq!(response, expected.as_bytes(), "range {range}");
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
            response,
            b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nContent-Range: bytes */11\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
            "range {range}"
        );
    }
}

#[test]
fn ignores_an_unsupported_range_unit() {
    let server = start_server(b"video-bytes");

    let response = request(
        server.address,
        "GET /media HTTP/1.1\r\nHost: localhost\r\nRange: items=0-1\r\nConnection: close\r\n\r\n",
    );

    assert_eq!(
        response,
        b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\nvideo-bytes"
    );
}

#[test]
fn handles_an_empty_media_file() {
    let server = start_server(b"");

    let full_response = request(
        server.address,
        "GET /media HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(
        full_response,
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
    );

    let range_response = request(
        server.address,
        "GET /media HTTP/1.1\r\nHost: localhost\r\nRange: bytes=0-0\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(
        range_response,
        b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nContent-Range: bytes */0\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
    );
}

#[test]
fn head_describes_media_without_sending_its_bytes() {
    let server = start_server(b"video-bytes");

    let response = request(
        server.address,
        "HEAD /media HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );

    assert_eq!(
        response,
        b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
    );
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
        assert_eq!(
            response,
            b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
            "range {range}"
        );
    }
}
