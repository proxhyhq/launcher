use std::process::{Child, Command};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

// Suppress the console window that Windows opens for child processes.
#[allow(clippy::missing_const_for_fn)]
pub fn no_window(_cmd: &mut Command) {
    #[cfg(windows)]
    _cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
}

// Since the Proxhy binary is a PyInstaller onefile bundle, the process spawned
// is a bootloader that extracts itself and runs the real interpreter as a child process
// so we need to kill the whole tree of processes

#[cfg(unix)]
pub fn prepare_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    // makes the spawned process its own process group leader, so we can
    // later signal the whole group (bootloader + whatever it spawns).
    cmd.process_group(0);
}

#[cfg(unix)]
#[allow(clippy::cast_possible_wrap)]
pub fn kill_tree(child: &Child) {
    let pgid = child.id() as i32;
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

#[cfg(windows)]
pub struct JobHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl JobHandle {
    pub fn new() -> Option<Self> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(info).cast(),
                u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap(),
            );
            if ok == 0 {
                windows_sys::Win32::Foundation::CloseHandle(handle);
                return None;
            }
            Some(Self(handle))
        }
    }

    // assigns the child (and, transitively, any processes it later spawns)
    // to this job so the whole tree can be killed at once.
    pub fn assign(&self, child: &Child) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle().cast()) != 0 }
    }

    pub fn terminate(&self) {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        unsafe {
            TerminateJobObject(self.0, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for JobHandle {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
