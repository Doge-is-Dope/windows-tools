#[cfg(not(target_os = "windows"))]
compile_error!("Build and run the host on Windows.");
mod jobs;
mod mcp;
mod tls;
mod windows;

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use windows_tools_core::{Lease, Revocation, Target, Toolset};

const USAGE: &str =
    "Use --install <IPv4>: once, as the session's user; the host then starts at each sign-in.";
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
        ["--install", address] => install(address),
        ["--serve", address] => {
            let state = tls::State::load()?;
            if state.token.len() < 32 {
                return Err("Token must contain at least 32 characters.".into());
            }
            if windows::session_id()? == 0 {
                return Err("Launch in the logged-in user's session, not session 0.".into());
            }
            let _instance = only_host()?;
            serve(state, address)
        }
        _ => Err(USAGE.into()),
    }
}

/// Makes this user's token and certificate once (a later run keeps them), starts the host now,
/// replacing any running one, and at each sign-in, and prints the line a controller is given.
fn install(address: &str) -> Result<(), Failure> {
    tls::State::create(address)?;
    let state = tls::State::load()?;
    let exe = std::env::current_exe()?;
    windows::start_at_sign_in(&exe, address)?;
    windows::end_other_hosts()?;
    windows::start(&exe, address)?;
    println!(
        "https://{}@{address}:{}/{}",
        state.token,
        tls::PORT,
        state.fingerprint
    );
    Ok(())
}

/// The session's one host: a host just replaced is waited for briefly while it ends.
fn only_host() -> Result<windows::Handle, Failure> {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        match windows::singleton() {
            Ok(instance) => return Ok(instance),
            Err(_) if Instant::now() < until => thread::sleep(Duration::from_millis(100)),
            Err(error) => return Err(error.into()),
        }
    }
}

/// Serves on `address`; requests must name it, as the controller's line does.
#[tokio::main]
async fn serve(state: tls::State, address: &str) -> Result<(), Failure> {
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
    let origin = format!("https://{address}:{}", tls::PORT);
    let adapter = mcp::Adapter::new(tools, origin, &state.token);
    tls::serve(mcp::router(adapter), address.parse()?, state.identity).await
}
