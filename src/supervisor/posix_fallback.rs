//! Cross-platform POSIX fallback process supervisor hooks

#[cfg(unix)]
mod unix_impl {
    use std::os::unix::process::CommandExt;

    pub fn configure_posix_command(cmd: &mut tokio::process::Command) {
        unsafe {
            cmd.pre_exec(|| {
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

    pub fn kill_posix_process_group(pgid: i32, sig: i32) -> std::io::Result<()> {
        let ret = unsafe { libc::killpg(pgid, sig) };
        if ret != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Configures process group isolation and parent death signal on Unix.
/// No-op on non-Unix platforms.
pub fn configure_posix_command(_cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    unix_impl::configure_posix_command(_cmd);
}

/// Sends a signal to a POSIX process group.
/// No-op on non-Unix platforms.
pub fn kill_posix_process_group(_pgid: i32, _sig: i32) -> std::io::Result<()> {
    #[cfg(unix)]
    return unix_impl::kill_posix_process_group(_pgid, _sig);
    #[cfg(not(unix))]
    Ok(())
}
