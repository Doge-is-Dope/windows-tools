//! The host's core: what a call is, who may make it, and how it reaches the session. It knows no
//! transport and no Windows.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{
    Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
pub mod output;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    /// Made new each time a host starts.
    pub host_instance: String,
    /// Moves on each time the lease is taken or ends.
    #[serde(default)]
    pub control_epoch: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
    pub delivery: String,
}
impl RpcError {
    pub fn unsent(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            delivery: "not_sent".into(),
        }
    }
    pub fn unknown(code: &str, message: &str) -> Self {
        Self {
            delivery: "unknown".into(),
            ..Self::unsent(code, message)
        }
    }
}
impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for RpcError {}
pub type RpcResult = Result<Value, RpcError>;

#[derive(Clone, Deserialize, Serialize)]
pub struct Response {
    pub id: String,
    pub result: Option<Value>,
    pub error: Option<RpcError>,
}
impl Response {
    pub fn new(id: String, result: RpcResult) -> Self {
        match result {
            Ok(value) => Self {
                id,
                result: Some(value),
                error: None,
            },
            Err(error) => Self {
                id,
                result: None,
                error: Some(error),
            },
        }
    }
}

/// Why an agent lease ended. Recorded so an unexpected loss can be diagnosed afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Revocation {
    /// No heartbeat renewed the lease before its deadline.
    Expired,
    /// The holder handed control back.
    Takeover,
    /// The Windows session stopped being connected and unlocked.
    DesktopUnavailable,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevocationRecord {
    pub reason: Revocation,
    /// The epoch whose lease ended; targets carrying it are now stale.
    pub ended_epoch: u64,
    pub at_unix_ms: u64,
}

struct LeaseState {
    epoch: u64,
    /// When the agent's lease runs out; `None` while a human owns the session.
    deadline: Option<Instant>,
    last_revocation: Option<RevocationRecord>,
}
pub struct Lease {
    timeout: Duration,
    state: Mutex<LeaseState>,
}
impl Lease {
    /// A host starts with the human owning the session.
    pub fn new(timeout: Duration) -> Self {
        let state = LeaseState {
            epoch: 0,
            deadline: None,
            last_revocation: None,
        };
        Self {
            timeout,
            state: Mutex::new(state),
        }
    }
    /// The state as of `now`: a lease whose deadline passed has ended, whoever looks first.
    fn at(&self, now: Instant) -> MutexGuard<'_, LeaseState> {
        let mut state = self.state.lock().unwrap();
        if state.deadline.is_some_and(|deadline| deadline <= now) {
            Self::end(&mut state, Revocation::Expired);
        }
        state
    }
    fn now(&self) -> MutexGuard<'_, LeaseState> {
        self.at(Instant::now())
    }
    /// Hands the session to the human, in a new epoch. Only an agent lease that actually ends
    /// is recorded, so a takeover of an already-human session keeps why the last one was lost.
    fn end(state: &mut LeaseState, reason: Revocation) {
        if state.deadline.take().is_some() {
            state.last_revocation = Some(RevocationRecord {
                reason,
                ended_epoch: state.epoch,
                at_unix_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64),
            });
        }
        state.epoch += 1;
    }
    pub fn epoch(&self) -> u64 {
        self.now().epoch
    }
    pub fn human(&self) -> bool {
        self.now().deadline.is_none()
    }
    pub fn owns(&self, epoch: u64) -> bool {
        let state = self.now();
        state.epoch == epoch && state.deadline.is_some()
    }
    /// Gives the agent the lease, in a new epoch.
    pub fn resume(&self, expected: u64) -> bool {
        let mut state = self.now();
        let current = state.epoch == expected;
        if current {
            state.epoch += 1;
            state.deadline = Some(Instant::now() + self.timeout);
        }
        current
    }
    pub fn renew(&self, expected: u64) -> bool {
        let mut state = self.now();
        let held = state.epoch == expected && state.deadline.is_some();
        if held {
            state.deadline = Some(Instant::now() + self.timeout);
        }
        held
    }
    pub fn takeover(&self, expected: u64) -> bool {
        let mut state = self.now();
        let current = state.epoch == expected;
        if current {
            Self::end(&mut state, Revocation::Takeover);
        }
        current
    }
    /// Ends an agent lease for `reason`. Nothing changes while a human owns the session.
    pub fn revoke(&self, reason: Revocation) {
        let mut state = self.now();
        if state.deadline.is_some() {
            Self::end(&mut state, reason);
        }
    }
    pub fn last_revocation(&self) -> Option<RevocationRecord> {
        self.now().last_revocation
    }
}

pub trait Execution: Send + Sync {
    /// Runs one command to its end, its deadline, or until `stop` says to stop it, and returns
    /// what it printed.
    fn run(&self, args: Value, stop: &dyn Fn() -> bool) -> RpcResult;
}
pub trait Session: Send + Sync {
    fn describe(&self, target: &Target, human: bool) -> Value;
}

/// The lease calls a connector makes; a model never sees them.
#[derive(Clone, Copy)]
pub enum Control {
    Resume,
    Heartbeat,
    Takeover,
}
impl Control {
    pub fn named(method: &str) -> Option<Self> {
        Some(match method {
            "control.resume" => Self::Resume,
            "control.heartbeat" => Self::Heartbeat,
            "control.takeover" => Self::Takeover,
            _ => return None,
        })
    }
}
pub const CAPABILITIES: [&str; 5] = [
    "describe",
    "control.resume",
    "control.heartbeat",
    "control.takeover",
    "exec.run",
];

pub struct Toolset {
    base: Target,
    pub lease: Lease,
    execution: Box<dyn Execution>,
    session: Box<dyn Session>,
    /// One action at a time.
    operations: Mutex<()>,
}
impl Toolset {
    pub fn new(
        base: Target,
        lease: Lease,
        execution: Box<dyn Execution>,
        session: Box<dyn Session>,
    ) -> Self {
        Self {
            base,
            lease,
            execution,
            session,
            operations: Mutex::new(()),
        }
    }
    pub fn target(&self) -> Target {
        Target {
            control_epoch: self.lease.epoch(),
            ..self.base.clone()
        }
    }
    pub fn describe(&self) -> Value {
        let mut value = self.session.describe(&self.target(), self.lease.human());
        value["capabilities"] = json!(CAPABILITIES);
        value["lastRevocation"] = json!(self.lease.last_revocation());
        value["desktopBusy"] = json!(self.operations.try_lock().is_err());
        value
    }
    /// A call names the target it is meant for; one for another session or epoch is refused.
    fn selected(&self, target: &Target) -> Result<(), RpcError> {
        if *target == self.target() {
            return Ok(());
        }
        Err(RpcError::unsent(
            "stale_session",
            "Describe and select the current session and control epoch.",
        ))
    }

    /// Lease transitions compare the selected epoch under the lease lock.
    pub fn control(&self, control: Control, target: &Target) -> RpcResult {
        self.selected(target)?;
        let epoch = target.control_epoch;
        let done = match control {
            Control::Heartbeat if !self.lease.renew(epoch) => {
                return Err(RpcError::unsent(
                    "lease_expired",
                    "The lease ended; describe the session and resume.",
                ));
            }
            Control::Heartbeat => true,
            Control::Takeover => self.lease.takeover(epoch),
            // Resuming a lease that is already held keeps it as it is, so two connectors of one
            // controller share it rather than taking it from each other.
            Control::Resume if self.lease.owns(epoch) => true,
            Control::Resume => {
                let interactive = self
                    .session
                    .describe(target, true)
                    .get("interactiveDesktop")
                    .and_then(Value::as_bool);
                if interactive == Some(false) {
                    return Err(RpcError::unsent(
                        "desktop_unavailable",
                        "Reconnect and unlock the Windows session before resuming.",
                    ));
                }
                self.lease.resume(epoch)
            }
        };
        if !done {
            return Err(RpcError::unsent(
                "stale_session",
                "The control epoch changed; describe the session again.",
            ));
        }
        Ok(self.describe())
    }

    /// One command at a time, and only under the lease: for `target`, or for the one current
    /// when the call arrives. `left` says the caller stopped waiting: the command is refused if
    /// it has not started and stopped if it is running. The lease is not touched.
    pub fn run(&self, args: Value, target: Option<Target>, left: &AtomicBool) -> RpcResult {
        let target = target.unwrap_or_else(|| self.target());
        self.selected(&target)?;
        let _operation = self.operations.lock().unwrap();
        let epoch = self.lease.epoch();
        if !self.lease.owns(epoch) {
            return Err(RpcError::unsent(
                "human_control",
                "A human owns this session.",
            ));
        }
        // Checked again after taking the operation lock: a takeover can race a queued call.
        self.selected(&target)?;
        if left.load(Ordering::SeqCst) {
            return Err(RpcError::unsent(
                "request_cancelled",
                "This request was cancelled before delivery.",
            ));
        }
        let stop = || left.load(Ordering::SeqCst) || !self.lease.owns(epoch);
        self.execution.run(args, &stop)
    }
}

pub fn invalid(message: &str) -> RpcError {
    RpcError::unsent("invalid_arguments", message)
}
#[cfg(test)]
mod tests;
