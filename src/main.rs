use std::{
    env,
    fs::File,
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process, thread,
};

const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;
const PLAYER_PAGE: &str = include_str!("../client/index.html");

#[derive(Debug)]
struct Configuration {
    media_path: PathBuf,
    bind_address: SocketAddr,
}

fn main() {
    let configuration = match Configuration::from_arguments(env::args().skip(1)) {
        Ok(configuration) => configuration,
        Err(message) => {
            eprintln!("{message}\n\nUsage: plpd --media <file> [--bind <address>]");
            process::exit(2);
        }
    };

    if let Err(error) = run(configuration) {
        eprintln!("plpd failed: {error}");
        process::exit(1);
    }
}

impl Configuration {
    fn from_arguments(arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut media_path = None;
        let mut bind_address = "127.0.0.1:8080"
            .parse::<SocketAddr>()
            .expect("literal address is valid");
        let mut arguments = arguments;

        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--media" => {
                    let value = arguments.next().ok_or("--media needs a file path")?;
                    media_path = Some(PathBuf::from(value));
                }
                "--bind" => {
                    let value = arguments.next().ok_or("--bind needs an address")?;
                    bind_address = value
                        .parse()
                        .map_err(|_| format!("invalid bind address: {value}"))?;
                }
                "--help" | "-h" => return Err("".to_owned()),
                _ => return Err(format!("unknown argument: {argument}")),
            }
        }

        let media_path = media_path.ok_or("--media is required")?;
        if !media_path.is_file() {
            return Err(format!(
                "media file does not exist: {}",
                media_path.display()
            ));
        }

        Ok(Self {
            media_path,
            bind_address,
        })
    }
}

fn run(configuration: Configuration) -> io::Result<()> {
    let listener = TcpListener::bind(configuration.bind_address)?;
    let address = listener.local_addr()?;
    eprintln!(
        "Serving {} at http://{address}/media",
        configuration.media_path.display()
    );

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let media_path = configuration.media_path.clone();
                thread::spawn(move || {
                    if let Err(error) = handle_connection(stream, &media_path) {
                        eprintln!("request failed: {error}");
                    }
                });
            }
            Err(error) => eprintln!("failed to accept connection: {error}"),
        }
    }

    Ok(())
}

fn handle_connection(mut stream: TcpStream, media_path: &Path) -> io::Result<()> {
    let request = read_request(&mut stream)?;
    let Some(request) = request else {
        return Ok(());
    };

    match (request.method.as_str(), request.target.as_str()) {
        ("GET", "/") | ("HEAD", "/") => serve_player(&mut stream, request.method == "HEAD"),
        ("GET", "/media") | ("HEAD", "/media") => serve_media(
            &mut stream,
            media_path,
            request.method == "HEAD",
            request.range.as_deref(),
        ),
        _ => write_response(
            &mut stream,
            "404 Not Found",
            &[("Content-Length", "0")],
            &[],
        ),
    }
}

fn serve_player(stream: &mut TcpStream, head_only: bool) -> io::Result<()> {
    let content_length = PLAYER_PAGE.len().to_string();
    write!(stream, "HTTP/1.1 200 OK\r\n")?;
    write!(stream, "Content-Length: {content_length}\r\n")?;
    write!(
        stream,
        "Content-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n"
    )?;
    if !head_only {
        stream.write_all(PLAYER_PAGE.as_bytes())?;
    }
    stream.shutdown(Shutdown::Write)
}

struct Request {
    method: String,
    target: String,
    range: Option<String>,
}

fn read_request(stream: &mut TcpStream) -> io::Result<Option<Request>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None);
    }
    if request_line.len() > MAX_REQUEST_HEADER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request headers are too large",
        ));
    }

    let mut header_bytes = request_line.len();
    let mut range = None;
    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        header_bytes += bytes_read;
        if header_bytes > MAX_REQUEST_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request headers are too large",
            ));
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("range") {
                range = Some(value.trim().to_owned());
            }
        }
    }

    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| invalid_request("missing HTTP method"))?;
    let target = parts
        .next()
        .ok_or_else(|| invalid_request("missing request target"))?;
    let version = parts
        .next()
        .ok_or_else(|| invalid_request("missing HTTP version"))?;
    if parts.next().is_some() || !version.starts_with("HTTP/") {
        return Err(invalid_request("malformed request line"));
    }

    Ok(Some(Request {
        method: method.to_owned(),
        target: target.to_owned(),
        range,
    }))
}

fn invalid_request(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn serve_media(
    stream: &mut TcpStream,
    media_path: &Path,
    head_only: bool,
    requested_range: Option<&str>,
) -> io::Result<()> {
    let mut media = File::open(media_path)?;
    let total_length = media.metadata()?.len();
    let byte_range = match requested_range {
        Some(value) => match ByteRange::parse(value, total_length) {
            Ok(range) => Some(range),
            Err(()) => return write_range_not_satisfiable(stream, total_length),
        },
        None => None,
    };

    let (status, content_length, content_range) = match byte_range {
        Some(range) => {
            media.seek(SeekFrom::Start(range.start))?;
            (
                "206 Partial Content",
                range.length(),
                Some(format!(
                    "bytes {}-{}/{}",
                    range.start, range.end, total_length
                )),
            )
        }
        None => ("200 OK", total_length, None),
    };

    let content_type = content_type_for(media_path);
    write!(stream, "HTTP/1.1 {status}\r\n")?;
    write!(stream, "Content-Length: {content_length}\r\n")?;
    if let Some(content_range) = content_range {
        write!(stream, "Content-Range: {content_range}\r\n")?;
    }
    write!(
        stream,
        "Content-Type: {content_type}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
    )?;

    if !head_only {
        io::copy(&mut media.take(content_length), stream)?;
    }
    stream.shutdown(Shutdown::Write)
}

#[derive(Clone, Copy)]
struct ByteRange {
    start: u64,
    end: u64,
}

impl ByteRange {
    fn parse(value: &str, total_length: u64) -> Result<Self, ()> {
        let value = value.strip_prefix("bytes=").ok_or(())?;
        if value.contains(',') {
            return Err(());
        }
        let (start, end) = value.split_once('-').ok_or(())?;

        if start.is_empty() {
            let suffix_length = end.parse::<u64>().map_err(|_| ())?;
            if suffix_length == 0 || total_length == 0 {
                return Err(());
            }
            let start = total_length.saturating_sub(suffix_length);
            return Ok(Self {
                start,
                end: total_length - 1,
            });
        }

        let start = start.parse::<u64>().map_err(|_| ())?;
        if start >= total_length {
            return Err(());
        }
        let end = if end.is_empty() {
            total_length - 1
        } else {
            end.parse::<u64>().map_err(|_| ())?.min(total_length - 1)
        };
        if end < start {
            return Err(());
        }

        Ok(Self { start, end })
    }

    fn length(self) -> u64 {
        self.end - self.start + 1
    }
}

fn write_range_not_satisfiable(stream: &mut TcpStream, total_length: u64) -> io::Result<()> {
    let length = total_length.to_string();
    write_response(
        stream,
        "416 Range Not Satisfiable",
        &[
            ("Content-Length", "0"),
            ("Content-Range", &format!("bytes */{length}")),
            ("Accept-Ranges", "bytes"),
        ],
        &[],
    )
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> io::Result<()> {
    write!(stream, "HTTP/1.1 {status}\r\n")?;
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "Connection: close\r\n\r\n")?;
    stream.write_all(body)?;
    stream.shutdown(Shutdown::Write)
}

fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("wav") => "audio/wav",
        _ => "application/octet-stream",
    }
}
