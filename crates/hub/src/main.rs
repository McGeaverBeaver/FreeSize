use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use freesize_hub::Config;
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    // Settings named for DiskTree (an Unraid template from before the
    // rename) count until they're renamed. Before any thread starts.
    for (key, value) in freesize_core::renamed::old_env() {
        // SAFETY: nothing else runs yet, so nothing reads the environment.
        unsafe { std::env::set_var(key, value) };
    }
    if std::env::args().nth(1).as_deref() == Some("health") {
        return health();
    }
    match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime.block_on(serve()),
        Err(err) => {
            eprintln!("freesize-hub: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn serve() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("FREESIZE_HUB_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let config = Config::parse();
    match freesize_hub::run(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("{err}");
            ExitCode::FAILURE
        }
    }
}

/// `freesize-hub health`: whether the Hub on this machine answers, for the
/// container's health check. Reads the listen address like the Hub does.
fn health() -> ExitCode {
    let listen: SocketAddr = std::env::var("FREESIZE_HUB_LISTEN")
        .ok()
        .and_then(|a| a.parse().ok())
        .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 9090)));
    let mut address = listen;
    if address.ip().is_unspecified() {
        address.set_ip(match address.ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        });
    }
    let answer = (|| -> std::io::Result<String> {
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.write_all(b"GET /api/status HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
        let mut answer = String::new();
        stream.take(4096).read_to_string(&mut answer)?;
        Ok(answer)
    })();
    match answer {
        Ok(text) if text.starts_with("HTTP/1.1 200") || text.starts_with("HTTP/1.0 200") => {
            println!("ok");
            ExitCode::SUCCESS
        }
        Ok(text) => {
            eprintln!("{address}: {}", text.lines().next().unwrap_or("no answer"));
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("{address}: {err}");
            ExitCode::FAILURE
        }
    }
}
