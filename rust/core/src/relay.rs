//! Another MCP server's tools, relayed: a server on newline-delimited JSON-RPC that starts on
//! first use and is ended, with everything it started, when a call is stopped or runs late.
use crate::{RpcError, RpcResult, invalid};
use serde_json::{Value, json};
use std::io::Write;
use std::sync::{
    Mutex, OnceLock,
    mpsc::{Receiver, RecvTimeoutError},
};
use std::time::{Duration, Instant};

pub const PROTOCOL: &str = "2025-11-25";
/// How long the server may take to start and to list its tools.
const START: Duration = Duration::from_secs(30);
/// How long one call may run.
pub const CALL: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);

/// A started server: where requests go, its replies, and what ends it when dropped.
pub struct Started {
    pub input: Box<dyn Write + Send>,
    pub replies: Receiver<Value>,
    pub process: Box<dyn Send>,
}
pub type Start = Box<dyn Fn() -> std::io::Result<Started> + Send + Sync>;

pub struct Relay {
    prefix: &'static str,
    start: Start,
    server: Mutex<Option<Server>>,
    listed: OnceLock<Vec<Value>>,
}
impl Relay {
    pub fn new(prefix: &'static str, start: Start) -> Self {
        Self {
            prefix,
            start,
            server: Mutex::new(None),
            listed: OnceLock::new(),
        }
    }
    pub fn owns(&self, name: &str) -> bool {
        name.starts_with(self.prefix)
    }
    /// The server's tools, each name prefixed. Listed once: they do not change while it runs.
    pub fn tools(&self) -> Result<&[Value], RpcError> {
        if let Some(listed) = self.listed.get() {
            return Ok(listed);
        }
        let listed = self.request("tools/list", json!({}), START, &|| false)?;
        let tools = listed["tools"]
            .as_array()
            .ok_or_else(|| RpcError::unsent("protocol_error", "The tools list had no tools."))?
            .iter()
            .filter_map(|tool| {
                let mut tool = tool.clone();
                tool["name"] = json!(format!("{}{}", self.prefix, tool["name"].as_str()?));
                Some(tool)
            })
            .collect();
        Ok(self.listed.get_or_init(|| tools))
    }
    /// The tools, if listed already.
    pub fn listed(&self) -> Option<&[Value]> {
        self.listed.get().map(Vec::as_slice)
    }
    /// Calls one tool by its prefixed name and returns the server's result as it came.
    pub fn call(&self, name: &str, args: Value, stop: &dyn Fn() -> bool) -> RpcResult {
        let original = name
            .strip_prefix(self.prefix)
            .ok_or_else(|| invalid("Unknown tool."))?;
        let params = json!({"name": original, "arguments": args});
        self.request("tools/call", params, CALL, stop)
    }
    /// A request that did not come back ends the server: what it left half done goes with it, and
    /// the next request starts it again.
    fn request(
        &self,
        method: &str,
        params: Value,
        within: Duration,
        stop: &dyn Fn() -> bool,
    ) -> RpcResult {
        let mut server = self.server.lock().unwrap();
        if server.is_none() {
            *server = Some(self.started(stop)?);
        }
        let deadline = Instant::now() + within;
        let result = server
            .as_mut()
            .unwrap()
            .request(method, params, deadline, stop);
        if result
            .as_ref()
            .is_err_and(|error| error.code != "invalid_arguments")
        {
            *server = None;
        }
        result
    }
    fn started(&self, stop: &dyn Fn() -> bool) -> Result<Server, RpcError> {
        let started = (self.start)().map_err(|error| {
            RpcError::unsent(
                "unavailable",
                &format!("The tools could not start: {error}"),
            )
        })?;
        let mut server = Server { started, next: 0 };
        let client = json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "windows-tools", "version": env!("CARGO_PKG_VERSION")}
        });
        // Nothing has run yet, whatever happens during the handshake.
        let unsent = |error: RpcError| RpcError::unsent(&error.code, &error.message);
        server
            .request("initialize", client, Instant::now() + START, stop)
            .map_err(unsent)?;
        server
            .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .map_err(unsent)?;
        Ok(server)
    }
}

struct Server {
    started: Started,
    next: u64,
}
impl Server {
    fn send(&mut self, message: Value) -> Result<(), RpcError> {
        let mut line = serde_json::to_vec(&message).expect("JSON");
        line.push(b'\n');
        let input = &mut self.started.input;
        input
            .write_all(&line)
            .and_then(|_| input.flush())
            .map_err(|_| {
                RpcError::unsent(
                    "unavailable",
                    "The tools had stopped; the next call starts them again.",
                )
            })
    }
    fn request(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
        stop: &dyn Fn() -> bool,
    ) -> RpcResult {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            if stop() {
                return Err(RpcError::unknown(
                    "cancelled",
                    "Stopped: the desktop changed hands.",
                ));
            }
            if Instant::now() >= deadline {
                return Err(RpcError::unknown(
                    "timed_out",
                    "Stopped: it did not finish in time.",
                ));
            }
            match self.started.replies.recv_timeout(POLL) {
                Ok(reply) if reply["id"] == id => {
                    if let Some(error) = reply.get("error") {
                        let message = error["message"].as_str().unwrap_or("The call was refused.");
                        return Err(invalid(message));
                    }
                    return Ok(reply["result"].clone());
                }
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(RpcError::unknown(
                        "exited",
                        "The tools stopped while it ran.",
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    };
    use std::thread;

    /// A server that answers each request after `delay`, or never for a call named "hang".
    struct Fake {
        starts: AtomicUsize,
        ended: Arc<AtomicUsize>,
    }
    struct Ends(Arc<AtomicUsize>);
    impl Drop for Ends {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    struct Pipe(mpsc::Sender<Vec<u8>>);
    impl Write for Pipe {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.send(bytes.to_vec()).map_err(std::io::Error::other)?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    fn relay() -> (Relay, Arc<Fake>) {
        let fake = Arc::new(Fake {
            starts: AtomicUsize::new(0),
            ended: Arc::new(AtomicUsize::new(0)),
        });
        let seen = fake.clone();
        let start: Start = Box::new(move || {
            seen.starts.fetch_add(1, Ordering::SeqCst);
            let (input, requests) = mpsc::channel::<Vec<u8>>();
            let (replies, received) = mpsc::channel();
            thread::spawn(move || {
                for line in requests {
                    let request: Value = serde_json::from_slice(&line).unwrap();
                    let Some(id) = request.get("id").cloned() else {
                        continue;
                    };
                    let reply = match (
                        request["method"].as_str(),
                        request["params"]["name"].as_str(),
                    ) {
                        (_, Some("hang")) => continue,
                        (_, Some("bad")) => json!({"id": id, "error": {"message": "no such tool"}}),
                        (Some("tools/list"), _) => {
                            json!({"id": id, "result": {"tools": [{"name": "click"}]}})
                        }
                        (Some("tools/call"), _) => {
                            json!({"id": id, "result": {"content": [], "args": request["params"]["arguments"]}})
                        }
                        _ => json!({"id": id, "result": {}}),
                    };
                    let _ =
                        replies.send(json!({"jsonrpc": "2.0", "method": "notifications/message"}));
                    let _ = replies.send(reply);
                }
            });
            Ok(Started {
                input: Box::new(Pipe(input)),
                replies: received,
                process: Box::new(Ends(seen.ended.clone())),
            })
        });
        (Relay::new("browser_", start), fake)
    }

    #[test]
    fn tools_are_listed_once_under_the_prefix_and_called_by_their_own_name() {
        let (relay, fake) = relay();
        assert_eq!(relay.tools().unwrap()[0]["name"], "browser_click");
        assert_eq!(relay.tools().unwrap().len(), 1);
        let result = relay
            .call("browser_click", json!({"uid": "1"}), &|| false)
            .unwrap();
        assert_eq!(result["args"]["uid"], "1");
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
        assert!(relay.owns("browser_click") && !relay.owns("windows_exec_run"));
    }

    #[test]
    fn a_refused_call_keeps_the_server_and_a_stopped_one_ends_it() {
        let (relay, fake) = relay();
        assert_eq!(
            relay
                .call("browser_bad", json!({}), &|| false)
                .unwrap_err()
                .delivery,
            "not_sent"
        );
        assert_eq!(fake.ended.load(Ordering::SeqCst), 0);
        let stop = AtomicBool::new(false);
        let stopped = thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(100));
                stop.store(true, Ordering::SeqCst);
            });
            relay.call("browser_hang", json!({}), &|| stop.load(Ordering::SeqCst))
        });
        let error = stopped.unwrap_err();
        assert_eq!(
            (error.code.as_str(), error.delivery.as_str()),
            ("cancelled", "unknown")
        );
        assert_eq!(fake.ended.load(Ordering::SeqCst), 1);
        // The next call starts the server again.
        assert!(relay.call("browser_click", json!({}), &|| false).is_ok());
        assert_eq!(fake.starts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_server_that_cannot_start_runs_nothing() {
        let relay = Relay::new(
            "browser_",
            Box::new(|| Err(std::io::Error::other("missing"))),
        );
        let error = relay
            .call("browser_click", json!({}), &|| false)
            .unwrap_err();
        assert_eq!(
            (error.code.as_str(), error.delivery.as_str()),
            ("unavailable", "not_sent")
        );
    }
}
