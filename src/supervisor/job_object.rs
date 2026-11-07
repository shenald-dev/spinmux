use std::io;

#[cfg(windows)]
use std::mem;
#[cfg(windows)]
use std::os::windows::io::RawHandle;
#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// Safe RAII wrapper for Windows Job Objects.
/// Enforces kernel-level child and grandchild process termination upon job close.
#[cfg(windows)]
pub struct WinJob {
    handle: HANDLE,
}

#[cfg(windows)]
unsafe impl Send for WinJob {}
#[cfg(windows)]
unsafe impl Sync for WinJob {}

#[cfg(windows)]
impl WinJob {
    /// Creates a new anonymous Windows Job Object configured with
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (0x00002000).
    pub fn new() -> io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        let ret = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };

        if ret == 0 {
            let err = io::Error::last_os_error();
            unsafe { CloseHandle(handle) };
            return Err(err);
        }

        Ok(Self { handle })
    }

    /// Associates a running process with this Job Object.
    /// All descendant processes spawned by this process will be encapsulated.
    pub fn assign_process(&self, process_handle: RawHandle) -> io::Result<()> {
        let ret = unsafe { AssignProcessToJobObject(self.handle, process_handle as HANDLE) };
        if ret == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Forcefully terminates all processes associated with this Job Object.
    pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
        let ret = unsafe { TerminateJobObject(self.handle, exit_code) };
        if ret == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Returns the raw Win32 HANDLE.
    pub fn raw_handle(&self) -> HANDLE {
        self.handle
    }

    /// Returns true if the underlying handle is valid.
    pub fn is_valid(&self) -> bool {
        !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE
    }
}

#[cfg(windows)]
impl Drop for WinJob {
    fn drop(&mut self) {
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

/// Fallback RAII stub for non-Windows platforms (Unix / macOS)
#[cfg(not(windows))]
pub struct WinJob;

#[cfg(not(windows))]
impl WinJob {
    pub fn new() -> io::Result<Self> {
        Ok(Self)
    }

    pub fn assign_process(&self, _process_handle: *mut std::ffi::c_void) -> io::Result<()> {
        Ok(())
    }

    pub fn terminate(&self, _exit_code: u32) -> io::Result<()> {
        Ok(())
    }

    pub fn is_valid(&self) -> bool {
        true
    }
}

pub type JobObject = WinJob;
