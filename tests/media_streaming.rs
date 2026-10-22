use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct RunningServer {
    child: Child,
    address: SocketAddr,
    media_path: PathBuf,
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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

    let probe = TcpListener::bind("127.0.0.1:0").expect("reserve local port");
    let address = probe.local_addr().expect("read local address");
    drop(probe);

    let child = Command::new(env!("CARGO_BIN_EXE_plpd"))
        .args([
            "--media",
            media_path.to_str().expect("media path is UTF-8"),
            "--bind",
            &address.to_string(),
        ])
        .spawn()
        .expect("start daemon");

    for _ in 0..40 {
        if TcpStream::connect_timeout(&address, Duration::from_millis(25)).is_ok() {
            return RunningServer {
                child,
                address,
                media_path,
            };
        }
        thread::sleep(Duration::from_millis(25));
    }

    panic!("daemon did not begin listening at {address}");
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
fn streams_a_requested_byte_range_for_seeking() {
    let server = start_server(b"video-bytes");

    let response = request(
        server.address,
        "GET /media HTTP/1.1\r\nHost: localhost\r\nRange: bytes=6-10\r\nConnection: close\r\n\r\n",
    );

    assert_eq!(
        response,
        b"HTTP/1.1 206 Partial Content\r\nContent-Length: 5\r\nContent-Range: bytes 6-10/11\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\nbytes"
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
