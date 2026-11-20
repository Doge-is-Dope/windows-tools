//! The browser tools: Chrome DevTools MCP, installed beside the host by
//! `scripts/install-browser-tools.ps1`, on stdio in a job that ends it and its Chrome together.
use crate::windows::Group;
use serde_json::Value;
use std::io::{self, BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use windows_tools_core::relay::{Relay, Started};

const NODE: &str = r"node\node.exe";
const SERVER: &str = r"browser\package\build\src\bin\chrome-devtools-mcp.js";
/// No usage statistics or page addresses leave the computer, and no update check runs at each start.
const FLAGS: [&str; 2] = ["--no-usage-statistics", "--no-performance-crux"];
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// The relay, when the browser tools are installed.
pub fn relay() -> Option<Relay> {
    let (node, server) = paths().ok()?;
    (node.is_file() && server.is_file()).then(|| Relay::new("browser_", Box::new(start)))
}

fn paths() -> io::Result<(PathBuf, PathBuf)> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| io::Error::other("No host folder."))?;
    Ok((dir.join(NODE), dir.join(SERVER)))
}

fn start() -> io::Result<Started> {
    let (node, server) = paths()?;
    let mut command = Command::new(node);
    command
        .arg(server)
        .args(FLAGS)
        .env("CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS", "1")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut group = Group::spawn(&mut command)?;
    let input = group.child.stdin.take().unwrap();
    let output = group.child.stdout.take().unwrap();
    let (sender, replies) = mpsc::channel();
    // Replies only: the server's notifications are not read.
    thread::spawn(move || {
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("id").is_some()
                && message.get("method").is_none()
                && sender.send(message).is_err()
            {
                break;
            }
        }
    });
    Ok(Started {
        input: Box::new(input),
        replies,
        process: Box::new(group),
    })
}
