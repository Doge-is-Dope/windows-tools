use serde_json::{Value, json};
use std::{
    io::{self, Write},
    mem::{size_of, zeroed},
    os::windows::io::AsRawHandle,
    path::Path,
    process::{Child, ChildStdin, Command},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread::{self, JoinHandle},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{
        JobObjects::*, RemoteDesktop::ProcessIdToSessionId, StationsAndDesktops::*,
        Threading::CreateEventW,
    },
};
use windows_tools_core::{Session, Target};

pub struct Handle(HANDLE);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
pub fn session_id() -> io::Result<u32> {
    let mut id = 0;
    if unsafe { ProcessIdToSessionId(std::process::id(), &mut id) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(id)
}
pub fn singleton() -> io::Result<Handle> {
    let name: Vec<u16> = "Local\\WindowsTools.SessionHost\0".encode_utf16().collect();
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    let owned = Handle(event);
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "A tools host already serves this session.",
        ));
    }
    Ok(owned)
}
/// Ends the other hosts of this session, and the commands they run.
pub fn end_other_hosts() -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    Command::new("taskkill")
        .args(["/F", "/FI", "IMAGENAME eq windows-tools-host.exe"])
        .args(["/FI", &format!("PID ne {}", std::process::id())])
        .args(["/FI", &format!("SESSION eq {}", session_id()?)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x08000000)
        .status()
        .map(|_| ())
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// Starts the host with no window at each sign-in of this user, with no administrator rights.
pub fn start_at_sign_in(exe: &Path, address: &str) -> io::Result<()> {
    let added = Command::new("reg")
        .args([
            "add",
            RUN_KEY,
            "/v",
            "Windows Tools",
            "/t",
            "REG_SZ",
            "/f",
            "/d",
        ])
        .arg(format!(
            "conhost.exe --headless \"{}\" --serve {address}",
            exe.display()
        ))
        .stdout(std::process::Stdio::null())
        .status()?;
    if added.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            "Could not register the host to start at sign-in.",
        ))
    }
}
/// Starts the host now, with no window and none of this process's handles: a caller reading
/// this process's output would otherwise wait for the host to end.
pub fn start(exe: &Path, address: &str) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CreateProcessW, PROCESS_INFORMATION,
        STARTUPINFOW,
    };
    let mut line = std::ffi::OsString::from("\"");
    line.push(exe);
    line.push(format!("\" --serve {address}"));
    let mut command: Vec<u16> = line.encode_wide().chain([0]).collect();
    let mut startup: STARTUPINFOW = unsafe { zeroed() };
    startup.cb = size_of::<STARTUPINFOW>() as u32;
    let mut process: PROCESS_INFORMATION = unsafe { zeroed() };
    let started = unsafe {
        CreateProcessW(
            std::ptr::null(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0, // inherits no handles
            CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process,
        )
    };
    if started == 0 {
        return Err(io::Error::last_os_error());
    }
    drop((Handle(process.hProcess), Handle(process.hThread)));
    Ok(())
}
pub fn interactive() -> bool {
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSConnectState, WTSFreeMemory, WTSQuerySessionInformationW,
    };
    let Ok(session) = session_id() else {
        return false;
    };
    let mut buffer = std::ptr::null_mut();
    let mut bytes = 0;
    let queried = unsafe {
        WTSQuerySessionInformationW(
            std::ptr::null_mut(),
            session,
            WTSConnectState,
            &mut buffer,
            &mut bytes,
        )
    } != 0;
    let connected =
        queried && !buffer.is_null() && bytes >= 4 && unsafe { *(buffer as *const i32) } == 0;
    if !buffer.is_null() {
        unsafe {
            WTSFreeMemory(buffer as *mut _);
        }
    }
    let desktop = unsafe { OpenInputDesktop(0, 0, 0x100) };
    let accessible = !desktop.is_null();
    if accessible {
        unsafe {
            CloseDesktop(desktop);
        }
    }
    connected && accessible
}
pub struct WindowsSession;
impl Session for WindowsSession {
    fn describe(&self, target: &Target, human: bool) -> Value {
        let accessible = interactive();
        json!({
            "target": target,
            "user": format!("{}\\{}", std::env::var("USERDOMAIN").unwrap_or_default(), std::env::var("USERNAME").unwrap_or_default()),
            "interactiveDesktop": accessible,
            "controlOwner": if human { "human" } else { "agent" },
        })
    }
}

struct InputWrite {
    result: Receiver<io::Result<ChildStdin>>,
    thread: JoinHandle<()>,
}

/// Keeps this process's own input and output from the programs it starts, so that one of them
/// still running cannot hold those pipes open.
pub fn keep_own_pipes() {
    let pipes = [
        io::stdin().as_raw_handle(),
        io::stdout().as_raw_handle(),
        io::stderr().as_raw_handle(),
    ];
    for pipe in pipes {
        unsafe {
            SetHandleInformation(pipe, HANDLE_FLAG_INHERIT, 0);
        }
    }
}

pub struct Group {
    pub child: Child,
    job: Handle,
    released: bool,
    writer: Option<InputWrite>,
}
impl Group {
    /// Starts `command` in a job that kills it and its descendants when the group drops.
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Handle(raw);
        limit(&job, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)?;
        let mut child = command.spawn()?;
        // The worker waits for its input before it starts anything, so nothing escapes the job.
        if unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle()) } == 0 {
            let error = io::Error::last_os_error();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Self {
            child,
            job,
            released: false,
            writer: None,
        })
    }
    /// Writing a full pipe must not block the thread supervising the process.
    pub fn write_input(&mut self, bytes: Vec<u8>) -> io::Result<()> {
        if !self.input_ready()? {
            return Err(io::Error::other("Worker input is still being written."));
        }
        let mut input = self
            .child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("Worker input is closed."))?;
        let (sender, result) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            let written = input
                .write_all(&bytes)
                .and_then(|_| input.flush())
                .map(|_| input);
            let _ = sender.send(written);
        });
        self.writer = Some(InputWrite { result, thread });
        Ok(())
    }
    pub fn input_ready(&mut self) -> io::Result<bool> {
        let Some(writer) = self.writer.as_ref() else {
            return Ok(true);
        };
        let result = match writer.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return Ok(false),
            Err(TryRecvError::Disconnected) => {
                Err(io::Error::other("Worker input writer stopped."))
            }
        };
        let writer = self.writer.take().unwrap();
        let _ = writer.thread.join();
        self.child.stdin = Some(result?);
        Ok(true)
    }
    pub fn kill(&mut self) {
        unsafe {
            TerminateJobObject(self.job.0, 1);
        }
    }
    /// Ends the group without killing: what the child left running keeps running, and may
    /// start programs outside the job as any program can.
    pub fn release(mut self) {
        self.released = limit(&self.job, JOB_OBJECT_LIMIT_BREAKAWAY_OK).is_ok();
    }
}
fn limit(job: &Handle, flags: u32) -> io::Result<()> {
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    limits.BasicLimitInformation.LimitFlags = flags;
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
impl Drop for Group {
    fn drop(&mut self) {
        if !self.released {
            self.kill();
        }
        let _ = self.child.wait();
        // Killing the whole job closes readers before waiting for a blocked pipe writer.
        if let Some(writer) = self.writer.take() {
            let _ = writer.thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };

    #[test]
    #[ignore = "child process for blocked-input tests"]
    fn unread_input_child() {
        thread::sleep(Duration::from_secs(30));
    }

    #[test]
    fn blocked_worker_input_remains_cancellable_and_is_joined_on_drop() {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "windows::tests::unread_input_child", "--ignored"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut group = Group::spawn(&mut command).unwrap();
        let started = Instant::now();
        group.write_input(vec![b'x'; 1024 * 1024]).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(100));
        assert!(!group.input_ready().unwrap());
        // Drop must kill the reader before joining the writer; otherwise this hangs for 30s.
        drop(group);
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
