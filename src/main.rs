//! Executable entry point for the private-library daemon.
//!
//! This module owns process-level concerns: command-line configuration,
//! validation, socket binding, startup messages, and exit codes. HTTP behavior
//! lives in `server`, while TCP and HTTP/1 connection handling live in
//! `transport`.

mod range;
mod server;
mod transport;

use std::{env, io, net::SocketAddr, path::PathBuf, process};
use tokio::net::TcpListener;

/// Configuration that must be known before the server starts.
#[derive(Debug)]
struct Configuration {
    /// The single local file exposed by the `/media` route.
    media_path: PathBuf,
    /// The local socket address on which the daemon listens.
    bind_address: SocketAddr,
}

/// Parses configuration, starts the daemon, and maps failures to process exit
/// codes suitable for a command-line program.
#[tokio::main]
async fn main() {
    let configuration = match Configuration::from_arguments(env::args().skip(1)) {
        Ok(configuration) => configuration,
        Err(message) => {
            eprintln!("{message}\n\nUsage: plpd --media <file> [--bind <address>]");
            process::exit(2);
        }
    };

    if let Err(error) = run(configuration).await {
        eprintln!("plpd failed: {error}");
        process::exit(1);
    }
}

impl Configuration {
    /// Parses `--media <file>` and the optional `--bind <address>` argument.
    ///
    /// Validation happens before the async server starts so request handlers can
    /// rely on the configured media path having referred to a file at startup.
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

/// Binds the configured listener, constructs the application router, and hands
/// both to the bounded transport loop.
async fn run(configuration: Configuration) -> io::Result<()> {
    let listener = TcpListener::bind(configuration.bind_address).await?;
    let address = listener.local_addr()?;
    eprintln!(
        "Serving {} at http://{address}/media",
        configuration.media_path.display()
    );

    transport::serve(listener, server::router(configuration.media_path)).await
}
