use serde_json::{Value, json};
use std::{
    io::{self, Write},
    mem::{size_of, zeroed},
    os::windows::io::AsRawHandle,
    process::{Child, ChildStdin, Command},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread::{self, JoinHandle},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{
        DataExchange::*,
        JobObjects::*,
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        RemoteDesktop::ProcessIdToSessionId,
        StationsAndDesktops::*,
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
/// Ends the other hosts of this session, and the commands they run, so this one can serve it.
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

const CF_UNICODETEXT: u32 = 13;
/// The clipboard held open; another program holding it is waited for briefly.
struct Clipboard;
impl Clipboard {
    fn open() -> io::Result<Self> {
        for _ in 0..40 {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Ok(Self);
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
        Err(io::Error::last_os_error())
    }
}
impl Drop for Clipboard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}
/// The clipboard's text, if it holds any.
pub fn clipboard_text() -> io::Result<Option<String>> {
    let _open = Clipboard::open()?;
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
    if handle.is_null() {
        return Ok(None);
    }
    let text = unsafe { GlobalLock(handle) } as *const u16;
    if text.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut length = 0;
    while length < 4096 && unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    let read = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    unsafe {
        GlobalUnlock(handle);
    }
    Ok(Some(read))
}
/// Replaces what the clipboard holds with `text`.
pub fn set_clipboard_text(text: &str) -> io::Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let _open = Clipboard::open()?;
    unsafe {
        if EmptyClipboard() == 0 {
            return Err(io::Error::last_os_error());
        }
        let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
        if memory.is_null() {
            return Err(io::Error::last_os_error());
        }
        let target = GlobalLock(memory) as *mut u16;
        if target.is_null() {
            return Err(io::Error::last_os_error());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
        GlobalUnlock(memory);
        // The clipboard owns the memory from here on.
        if SetClipboardData(CF_UNICODETEXT, memory).is_null() {
            return Err(io::Error::last_os_error());
        }
    }
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
