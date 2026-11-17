#[cfg(not(target_os = "windows"))]
compile_error!("Build and run the host on Windows.");
mod jobs;
mod mcp;
mod tls;
mod windows;

use std::{
    io,
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use windows_tools_core::{Lease, Revocation, Target, Toolset};

/// `connection.json`, as `--prepare` writes it. Other fields in the file are ignored.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    token_file: PathBuf,
    url: String,
    certificate_file: PathBuf,
    key_file: PathBuf,
}

/// What a host serves with, however it was started.
struct Start {
    token: String,
    url: String,
    identity: tls::Identity,
}
impl Start {
    /// Refuses what the host cannot serve with, before anything else happens.
    fn check(&self) -> Result<(), Failure> {
        if !self.url.starts_with("https://") {
            return Err("The host needs an https:// URL.".into());
        }
        if self.token.len() < 32 {
            return Err("Token must contain at least 32 characters.".into());
        }
        if windows::session_id()? == 0 {
            return Err("Launch in the logged-in user's session, not session 0.".into());
        }
        Ok(())
    }
}

const USAGE: &str =
    "Use --serve <connection.json>, --prepare <new directory> <IPv4>, or --attach <IPv4> <port>.";
/// How a controller hands over the token for `--attach`, and how the host answers, on the
/// clipboard of the session they share.
const TOKEN_PREFIX: &str = "windows-tools-token:";
const FINGERPRINT_PREFIX: &str = "windows-tools-fingerprint:";
/// How long a lease lasts without a heartbeat.
const LEASE: Duration = Duration::from_secs(5);

type Failure = Box<dyn std::error::Error>;

fn main() -> Result<(), Failure> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--exec-worker"] => std::process::exit(jobs::exec_worker().unwrap_or_else(|error| {
            eprintln!("{error}");
            1
        })),
        ["--prepare", directory, address] => tls::prepare(&PathBuf::from(directory), address),
        ["--serve", file] => {
            let config: Config = serde_json::from_slice(&std::fs::read(file)?)?;
            let start = Start {
                token: std::fs::read_to_string(&config.token_file)?
                    .trim()
                    .to_owned(),
                url: config.url,
                identity: tls::Identity::from_files(&config.certificate_file, &config.key_file)?,
            };
            start.check()?;
            let _instance = windows::singleton()?;
            serve(start, None)
        }
        ["--attach", address, port] => attach(address, port),
        _ => Err(USAGE.into()),
    }
}

/// Serves for the controller that just put a token on this session's clipboard: takes the
/// token, replaces whatever host served the session, and once listening answers with the
/// fingerprint of a certificate made for this run. Nothing is written to disk, and the token
/// dies with the process.
fn attach(address: &str, port: &str) -> Result<(), Failure> {
    let port: u16 = port.parse()?;
    let (identity, fingerprint) = tls::Identity::ephemeral(address)?;
    let start = Start {
        token: taken_token()?,
        url: format!("https://{address}:{port}"),
        identity,
    };
    // Checked before the serving host is ended, so a bad start leaves it serving.
    start.check()?;
    let _instance = match windows::singleton() {
        Ok(instance) => instance,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            windows::end_other_hosts()?;
            let until = Instant::now() + Duration::from_secs(10);
            loop {
                match windows::singleton() {
                    Ok(instance) => break instance,
                    Err(_) if Instant::now() < until => thread::sleep(Duration::from_millis(100)),
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Err(error) => return Err(error.into()),
    };
    serve(start, Some(fingerprint))
}

/// The token a controller left on the clipboard, waited for briefly: over a remote desktop the
/// clipboard's content arrives when it is first asked for.
fn taken_token() -> Result<String, Failure> {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let found = windows::clipboard_text()?
            .and_then(|text| text.trim().strip_prefix(TOKEN_PREFIX).map(str::to_owned));
        match found {
            Some(token) => return Ok(token),
            None if Instant::now() < until => thread::sleep(Duration::from_millis(200)),
            None => return Err("No token on the clipboard.".into()),
        }
    }
}

/// Serves `start`. A `fingerprint` goes on the clipboard once the host listens: putting it
/// there also takes the token off.
#[tokio::main]
async fn serve(start: Start, fingerprint: Option<String>) -> Result<(), Failure> {
    let base = Target {
        host_instance: uuid::Uuid::new_v4().simple().to_string(),
        control_epoch: 0,
    };
    let tools = Arc::new(Toolset::new(
        base,
        Lease::new(LEASE),
        Box::new(jobs::Jobs::default()),
        Box::new(windows::WindowsSession),
    ));
    // A lease ends when its heartbeats stop, or when the session is locked or disconnected.
    let watched = tools.clone();
    tokio::spawn(async move {
        // The lease ends by its own clock; only the session's state needs watching.
        let mut tick = tokio::time::interval(Duration::from_millis(240));
        loop {
            tick.tick().await;
            if !windows::interactive() {
                watched.lease.revoke(Revocation::DesktopUnavailable);
            }
        }
    });
    let adapter = mcp::Adapter::new(tools, start.url.clone(), &start.token);
    let listening = move || match fingerprint {
        Some(fingerprint) => {
            windows::set_clipboard_text(&format!("{FINGERPRINT_PREFIX}{fingerprint}"))
        }
        None => Ok(()),
    };
    tls::serve(mcp::router(adapter), &start.url, start.identity, listening).await
}
