use super::*;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::thread;

#[derive(Default)]
struct Fake {
    runs: AtomicUsize,
    stopped: AtomicBool,
    interactive: AtomicBool,
}
impl Execution for Arc<Fake> {
    fn run(&self, _: Value, stop: &dyn Fn() -> bool) -> RpcResult {
        self.runs.fetch_add(1, Ordering::SeqCst);
        self.stopped.store(stop(), Ordering::SeqCst);
        Ok(json!({"exitCode":0}))
    }
}
impl Session for Arc<Fake> {
    fn describe(&self, target: &Target, human: bool) -> Value {
        json!({"target": target, "controlOwner": if human { "human" } else { "agent" },
               "interactiveDesktop": self.interactive.load(Ordering::SeqCst)})
    }
}

fn setup() -> (Toolset, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    fake.interactive.store(true, Ordering::SeqCst);
    let base = Target {
        host_instance: "a".into(),
        control_epoch: 0,
    };
    let lease = Lease::new(Duration::from_secs(5));
    let tools = Toolset::new(base, lease, Box::new(fake.clone()), Box::new(fake.clone()));
    (tools, fake)
}
fn run(d: &Toolset, target: &Target) -> RpcResult {
    d.run(
        json!({"command":"hostname"}),
        Some(target.clone()),
        &AtomicBool::new(false),
    )
}
fn code(result: RpcResult) -> String {
    result.unwrap_err().code
}

#[test]
fn a_host_starts_with_a_human_and_runs_nothing_until_resumed() {
    let (d, f) = setup();
    let described = d.describe();
    assert_eq!(described["controlOwner"], "human");
    assert_eq!(code(run(&d, &d.target())), "human_control");
    assert!(d.control(Control::Resume, &d.target()).is_ok());
    assert_eq!(run(&d, &d.target()).unwrap()["exitCode"], 0);
    // A call that names no target is made for the current one.
    assert!(d.run(json!({}), None, &AtomicBool::new(false)).is_ok());
    assert_eq!(f.runs.load(Ordering::SeqCst), 2);
}
#[test]
fn a_caller_that_left_is_refused_before_delivery_and_stopped_after() {
    let (d, f) = setup();
    d.control(Control::Resume, &d.target()).unwrap();
    let left = AtomicBool::new(true);
    let refused = d.run(json!({}), Some(d.target()), &left);
    assert_eq!(code(refused), "request_cancelled");
    assert_eq!(f.runs.load(Ordering::SeqCst), 0);
    assert!(run(&d, &d.target()).is_ok());
    assert!(!f.stopped.load(Ordering::SeqCst));
    // The lease stays.
    assert!(d.control(Control::Heartbeat, &d.target()).is_ok());
}
#[test]
fn a_call_for_another_epoch_is_refused() {
    let (d, _) = setup();
    let before = d.target();
    d.control(Control::Resume, &before).unwrap();
    assert_eq!(code(run(&d, &before)), "stale_session");
    let held = d.target();
    assert!(d.control(Control::Heartbeat, &held).is_ok());
    assert!(d.control(Control::Takeover, &held).is_ok());
    assert!(d.lease.human() && !d.lease.owns(held.control_epoch));
    assert_eq!(code(d.control(Control::Heartbeat, &held)), "stale_session");
    assert_eq!(
        d.lease.last_revocation().unwrap().reason,
        Revocation::Takeover
    );
}
#[test]
fn resuming_a_held_lease_keeps_it() {
    let (d, _) = setup();
    d.control(Control::Resume, &d.target()).unwrap();
    let target = d.target();
    // A second connector of the same controller joins the lease instead of taking it.
    let again = d.control(Control::Resume, &target).unwrap();
    assert_eq!(again["target"], json!(target));
    assert_eq!(d.target(), target);
}
#[test]
fn a_lease_ends_without_heartbeats_and_what_runs_is_told_to_stop() {
    let (d, _) = setup();
    d.control(Control::Resume, &d.target()).unwrap();
    let held = d.target();
    drop(d.lease.at(Instant::now() + Duration::from_secs(4)));
    assert!(d.lease.owns(held.control_epoch));
    drop(d.lease.at(Instant::now() + Duration::from_secs(6)));
    assert!(d.lease.human() && !d.lease.owns(held.control_epoch));
    assert_eq!(
        d.lease.last_revocation().unwrap().reason,
        Revocation::Expired
    );
    assert_eq!(code(run(&d, &held)), "stale_session");
    // Nothing to revoke: a human already has it.
    let human = d.target();
    d.lease.revoke(Revocation::DesktopUnavailable);
    assert_eq!(d.target(), human);
}
#[test]
fn a_locked_session_cannot_be_resumed() {
    let (d, f) = setup();
    f.interactive.store(false, Ordering::SeqCst);
    assert_eq!(
        code(d.control(Control::Resume, &d.target())),
        "desktop_unavailable"
    );
    assert!(d.lease.human());
}
/// Holds the first command until told, so a second one queues behind it.
struct Held(Mutex<mpsc::Receiver<()>>, AtomicUsize);
impl Execution for Arc<Held> {
    fn run(&self, _: Value, _: &dyn Fn() -> bool) -> RpcResult {
        if self.1.fetch_add(1, Ordering::SeqCst) == 0 {
            self.0.lock().unwrap().recv().unwrap();
        }
        Ok(json!({}))
    }
}
#[test]
fn a_queued_call_without_a_target_runs_only_in_the_epoch_it_arrived_in() {
    let (old, fake) = setup();
    let (release, wait) = mpsc::channel();
    let held = Arc::new(Held(Mutex::new(wait), AtomicUsize::new(0)));
    let d = Toolset::new(
        old.target(),
        Lease::new(Duration::from_secs(5)),
        Box::new(held.clone()),
        Box::new(fake),
    );
    d.control(Control::Resume, &d.target()).unwrap();
    let never = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| d.run(json!({}), None, &never));
        while d.describe()["desktopBusy"] != true {
            thread::yield_now();
        }
        let queued = scope.spawn(|| d.run(json!({}), None, &never));
        thread::sleep(Duration::from_millis(50));
        // The session changes hands and is resumed while the second call waits.
        d.control(Control::Takeover, &d.target()).unwrap();
        d.control(Control::Resume, &d.target()).unwrap();
        release.send(()).unwrap();
        assert_eq!(code(queued.join().unwrap()), "stale_session");
    });
    assert_eq!(held.1.load(Ordering::SeqCst), 1);
}
