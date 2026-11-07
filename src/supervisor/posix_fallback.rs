//! Cross-platform POSIX fallback process supervisor hooks

/// Configures process group isolation and parent death signal on Unix
pub fn configure_posix_command(_cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            _cmd.pre_exec(|| {
                // Place child process into its own process group
                libc::setpgid(0, 0);

                #[cfg(target_os = "linux")]
                {
                    // Ensure process group dies if parent supervisor dies
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                }

                Ok(())
            });
        }
    }
}

/// Sends a signal to a POSIX process group
pub fn kill_posix_process_group(_pgid: i32, _sig: i32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let ret = unsafe { libc::killpg(_pgid, _sig) };
        if ret != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}
