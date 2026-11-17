use crate::windows::{Group, keep_own_pipes};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::os::windows::process::CommandExt;
use std::{
    collections::HashMap,
    io::Read,
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use windows_tools_core::{
    Execution, RpcError, RpcResult, invalid,
    output::{KEPT, Tail, Utf8Stream},
};

/// What the worker process is told to run.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Spec {
    executable: String,
    arguments: Vec<String>,
    working_directory: String,
    environment: HashMap<String, String>,
}
/// Applied when `exec.run` omits `timeoutMs`; transports advertise the same value.
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_MS
}
/// `exec.run`: one PowerShell command, waited for.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Script {
    command: String,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}
/// The longest command, in UTF-16 units: it travels in an environment variable, which Windows
/// caps at 32,767.
pub const MAX_COMMAND: usize = 32_000;
const COMMAND_VARIABLE: &str = "WT_COMMAND";
/// Runs the command from an environment variable: no file for an execution policy to refuse, and
/// no quoting between the caller's text and PowerShell. Whether its last statement succeeded is
/// read inside the command's own text, the only place `$?` still says so; when it failed the
/// exit code is that of the program it ran, or 1.
const RUNNER: &str = "[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);\
$ProgressPreference='SilentlyContinue';$global:LASTEXITCODE=0;$global:WT_OK=$false;\
Invoke-Expression ($env:WT_COMMAND+[char]10+';$global:WT_OK=$?');\
if(-not $WT_OK){if($LASTEXITCODE){exit $LASTEXITCODE};exit 1}";
struct Data {
    state: String,
    exit_code: Option<i32>,
    error: Option<String>,
    output: Tail,
}
type Job = Mutex<Data>;
/// Runs PowerShell commands. The toolset runs one at a time.
pub struct Jobs {
    /// The arguments that make this executable the worker.
    worker: &'static [&'static str],
}
impl Default for Jobs {
    fn default() -> Self {
        Self {
            worker: &["--exec-worker"],
        }
    }
}
impl Execution for Jobs {
    fn run(&self, args: Value, stop: &dyn Fn() -> bool) -> RpcResult {
        let script: Script =
            serde_json::from_value(args).map_err(|_| invalid("Invalid command schema."))?;
        if script.command.trim().is_empty()
            || script.command.contains('\0')
            || script.command.encode_utf16().count() > MAX_COMMAND
            || !(1000..=600000).contains(&script.timeout_ms)
        {
            return Err(invalid("Invalid command or timeout."));
        }
        let (Ok(windows), Ok(home)) = (std::env::var("SystemRoot"), std::env::var("USERPROFILE"))
        else {
            return Err(RpcError::unsent(
                "unavailable",
                "The session has no Windows or profile directory.",
            ));
        };
        let spec = Spec {
            executable: format!(r"{windows}\System32\WindowsPowerShell\v1.0\powershell.exe"),
            arguments: [
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-OutputFormat",
                "Text",
            ]
            .into_iter()
            .chain(["-Command", RUNNER])
            .map(str::to_owned)
            .collect(),
            working_directory: home,
            environment: HashMap::from([(COMMAND_VARIABLE.into(), script.command)]),
        };
        let job = Arc::new(Mutex::new(Data {
            state: "running".into(),
            exit_code: None,
            error: None,
            output: Tail::new(KEPT),
        }));
        let started = Instant::now();
        execute(
            &job,
            self.worker,
            &spec,
            Duration::from_millis(script.timeout_ms),
            stop,
        );
        let mut d = job.lock().unwrap();
        let (output, cut) = d.output.end();
        Ok(json!({
            "state": d.state,
            "exitCode": d.exit_code,
            "error": d.error,
            "durationMs": started.elapsed().as_millis() as u64,
            "output": output,
            "outputTruncated": cut,
        }))
    }
}
fn pump<R: Read>(mut stream: R, job: Arc<Job>) {
    let mut decoder = Utf8Stream::default();
    let mut bytes = [0; 4096];
    loop {
        match stream.read(&mut bytes) {
            Ok(0) => {
                job.lock().unwrap().output.push(&decoder.push(&[], true));
                break;
            }
            Ok(n) => job
                .lock()
                .unwrap()
                .output
                .push(&decoder.push(&bytes[..n], false)),
            Err(_) => break,
        }
    }
}
/// Runs the job's program to its end, its deadline, a cancellation or the loss of the lease,
/// and records how it ended. A command that is stopped takes everything it started with it; one
/// that ends on its own leaves running what it started.
fn execute(
    job: &Arc<Job>,
    worker: &[&str],
    spec: &Spec,
    timeout: Duration,
    stop: &dyn Fn() -> bool,
) {
    let result = (|| -> Result<(&'static str, i32), Box<dyn std::error::Error>> {
        let until = Instant::now() + timeout;
        if stop() {
            return Ok(("cancelled", 1));
        }
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(worker)
            .creation_flags(0x08000000)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut group = Group::spawn(&mut command)?;
        let stdout = group.child.stdout.take().unwrap();
        let stderr = group.child.stderr.take().unwrap();
        let j = job.clone();
        let out = thread::spawn(move || pump(stdout, j));
        let j = job.clone();
        let err = thread::spawn(move || pump(stderr, j));
        if !stop() {
            let mut input = serde_json::to_vec(spec)?;
            input.push(b'\n');
            group.write_input(input)?;
        } else {
            group.kill();
        }
        let mut stopped = None;
        let mut input_error = None;
        let status = loop {
            if stop() {
                stopped = Some("cancelled");
                group.kill();
            } else if Instant::now() >= until {
                stopped = Some("timed_out");
                group.kill();
            }
            if stopped.is_none()
                && input_error.is_none()
                && let Err(error) = group.input_ready()
            {
                input_error = Some(error);
                group.kill();
            }
            if let Some(status) = group.child.try_wait()? {
                break status;
            }
            thread::sleep(Duration::from_millis(20));
        };
        if stopped.is_some() || input_error.is_some() {
            group.kill();
        } else {
            group.release();
        }
        // Only the worker held the pipes, and it is gone.
        let _ = out.join();
        let _ = err.join();
        if let Some(error) = input_error {
            return Err(error.into());
        }
        let code = status.code().unwrap_or(1);
        Ok((
            stopped.unwrap_or(if code == 0 { "succeeded" } else { "failed" }),
            code,
        ))
    })();
    let mut d = job.lock().unwrap();
    match result {
        Ok((state, code)) => {
            d.state = state.into();
            d.exit_code = Some(code);
        }
        Err(e) => {
            d.state = "failed".into();
            d.error = Some(e.to_string());
        }
    }
}
/// How long the worker reads on after the command ends. What the command printed is already in
/// the pipes; a program it left running may hold them open for as long as it runs.
const DRAIN: Duration = Duration::from_secs(1);
pub fn exec_worker() -> Result<i32, Box<dyn std::error::Error>> {
    keep_own_pipes();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let s: Spec = serde_json::from_str(&line)?;
    let mut cmd = Command::new(s.executable);
    cmd.args(s.arguments)
        .current_dir(s.working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000);
    cmd.envs(s.environment);
    let mut child = cmd.spawn()?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let (done, drained) = mpsc::channel();
    let copied = done.clone();
    thread::spawn(move || {
        let _ = std::io::copy(&mut out, &mut std::io::stdout());
        let _ = copied.send(());
    });
    thread::spawn(move || {
        let _ = std::io::copy(&mut err, &mut std::io::stderr());
        let _ = done.send(());
    });
    let code = child.wait()?.code().unwrap_or(1);
    let until = Instant::now() + DRAIN;
    while drained
        .recv_timeout(until.saturating_duration_since(Instant::now()))
        .is_ok()
    {}
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    const SLEEPER: &str =
        "Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep 60' -PassThru";
    /// Runs commands with this test binary as the worker.
    fn jobs() -> Jobs {
        Jobs {
            worker: &["--exact", "jobs::tests::worker", "--ignored", "--nocapture"],
        }
    }
    fn run(jobs: &Jobs, command: &str, timeout_ms: u64, stop: &dyn Fn() -> bool) -> Value {
        jobs.run(json!({"command": command, "timeoutMs": timeout_ms}), stop)
            .unwrap()
    }
    /// The process id a command printed last.
    fn started(result: &Value) -> u32 {
        let output = result["output"].as_str().unwrap();
        output.split_whitespace().last().unwrap().parse().unwrap()
    }
    /// Whether the process still ran; it is ended either way.
    fn still_ran(jobs: &Jobs, pid: u32) -> bool {
        thread::sleep(Duration::from_millis(500));
        let command = format!(
            "$p = Get-Process -Id {pid} -ErrorAction SilentlyContinue; [bool]$p; if ($p) {{ Stop-Process -Id {pid} }}"
        );
        let seen = run(jobs, &command, 30_000, &|| false);
        seen["output"].as_str().unwrap().contains("True")
    }
    #[test]
    #[ignore = "the worker process for the tests below"]
    fn worker() {
        std::process::exit(exec_worker().unwrap_or(1));
    }
    #[test]
    fn a_program_the_command_started_outlives_its_normal_end() {
        let jobs = jobs();
        for window in ["-WindowStyle Hidden", "-NoNewWindow"] {
            let began = Instant::now();
            let result = run(&jobs, &format!("({SLEEPER} {window}).Id"), 30_000, &|| {
                false
            });
            assert_eq!(result["state"], "succeeded", "{window}: {result}");
            assert!(began.elapsed() < Duration::from_secs(20), "{window}");
            assert!(still_ran(&jobs, started(&result)), "{window}");
        }
    }
    #[test]
    fn output_without_a_line_end_arrives_while_a_program_holds_the_pipes() {
        let jobs = jobs();
        let command =
            format!("$p = {SLEEPER} -NoNewWindow; [Console]::Out.Write(\"end $($p.Id)\")");
        let result = run(&jobs, &command, 30_000, &|| false);
        assert!(
            result["output"].as_str().unwrap().contains("end "),
            "{result}"
        );
        assert!(still_ran(&jobs, started(&result)));
    }
    #[test]
    fn a_stopped_command_takes_what_it_started_with_it() {
        let jobs = jobs();
        let command = format!("({SLEEPER} -WindowStyle Hidden).Id; Start-Sleep 60");
        let late = run(&jobs, &command, 15_000, &|| false);
        assert_eq!(late["state"], "timed_out", "{late}");
        assert!(!still_ran(&jobs, started(&late)));
        let left = AtomicBool::new(false);
        let cancelled = thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_secs(15));
                left.store(true, Ordering::SeqCst);
            });
            run(&jobs, &command, 60_000, &|| left.load(Ordering::SeqCst))
        });
        assert_eq!(cancelled["state"], "cancelled", "{cancelled}");
        assert!(!still_ran(&jobs, started(&cancelled)));
    }
    #[test]
    fn a_command_is_refused_before_anything_runs_when_it_is_empty_or_too_long() {
        for command in [" ".to_owned(), "x".repeat(MAX_COMMAND + 1)] {
            let refused = Jobs::default()
                .run(json!({"command": command}), &|| false)
                .unwrap_err();
            assert_eq!(refused.code, "invalid_arguments");
        }
        let refused = Jobs::default().run(json!({"command":"hostname","timeoutMs":1}), &|| false);
        assert_eq!(refused.unwrap_err().code, "invalid_arguments");
    }
}
