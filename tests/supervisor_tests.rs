use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use spinmux::discovery::{ServiceDefinition, ServiceSource};
use spinmux::supervisor::{ProcessStatus, Supervisor};

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }

    let mut exit_code: u32 = 0;
    let ok = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    unsafe { CloseHandle(handle) };

    if ok == 0 {
        return false;
    }
    exit_code == STILL_ACTIVE as u32
}

fn create_mock_service(name: &str, command: &str, args: &[&str]) -> ServiceDefinition {
    ServiceDefinition {
        id: name.to_string(),
        name: name.to_string(),
        command: command.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        working_dir: PathBuf::from("."),
        env: HashMap::new(),
        source: ServiceSource::Procfile {
            path: PathBuf::from("Procfile"),
            variant: "dev".to_string(),
        },
        target_port: None,
        is_http: false,
        subdomain: name.to_string(),
    }
}

#[tokio::test]
async fn test_spawn_mock_process_and_capture_pid() {
    let supervisor = Supervisor::new().expect("Supervisor initialization failed");

    #[cfg(windows)]
    let svc = create_mock_service(
        "mock_pinger",
        "cmd.exe",
        &["/C", "ping -n 10 127.0.0.1 >nul"],
    );
    #[cfg(not(windows))]
    let svc = create_mock_service("mock_pinger", "sleep", &["10"]);

    supervisor.add_service(svc, None).await.unwrap();

    supervisor.start_service("mock_pinger").await.unwrap();

    // Verify status is Running(pid)
    let status = supervisor.status("mock_pinger").await.unwrap();
    assert!(
        status.is_running(),
        "Expected process to be running, got {:?}",
        status
    );

    let pid = status.pid().expect("PID must be available");
    assert!(pid > 0, "PID must be positive integer");

    #[cfg(windows)]
    assert!(
        is_process_alive(pid),
        "Process with PID {} should be alive in OS",
        pid
    );

    // Stop process
    supervisor.stop_service("mock_pinger").await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let stopped_status = supervisor.status("mock_pinger").await.unwrap();
    assert!(
        stopped_status.is_stopped(),
        "Expected process to be stopped, got {:?}",
        stopped_status
    );

    #[cfg(windows)]
    assert!(
        !is_process_alive(pid),
        "Process with PID {} must be dead after stop",
        pid
    );
}

#[tokio::test]
async fn test_process_stop_and_restart() {
    let supervisor = Supervisor::new().expect("Supervisor initialization failed");

    #[cfg(windows)]
    let svc = create_mock_service(
        "restartable",
        "cmd.exe",
        &["/C", "ping -n 20 127.0.0.1 >nul"],
    );
    #[cfg(not(windows))]
    let svc = create_mock_service("restartable", "sleep", &["20"]);

    supervisor.add_service(svc, None).await.unwrap();
    supervisor.start_service("restartable").await.unwrap();

    let pid1 = supervisor
        .status("restartable")
        .await
        .unwrap()
        .pid()
        .unwrap();
    assert!(pid1 > 0);

    // Stop service
    supervisor.stop_service("restartable").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(supervisor.status("restartable").await.unwrap().is_stopped());

    // Restart service
    supervisor.restart_service("restartable").await.unwrap();
    let status2 = supervisor.status("restartable").await.unwrap();
    assert!(status2.is_running());

    let pid2 = status2.pid().unwrap();
    assert!(pid2 > 0);
    assert_ne!(
        pid1, pid2,
        "Restarting service must allocate a new process PID"
    );

    supervisor.stop_service("restartable").await.unwrap();
}

#[tokio::test]
async fn test_process_exit_code_detection() {
    let supervisor = Supervisor::new().expect("Supervisor initialization failed");

    #[cfg(windows)]
    let clean_svc = create_mock_service("clean_exit", "cmd.exe", &["/C", "exit 0"]);
    #[cfg(not(windows))]
    let clean_svc = create_mock_service("clean_exit", "sh", &["-c", "exit 0"]);

    supervisor.add_service(clean_svc, None).await.unwrap();
    supervisor.start_service("clean_exit").await.unwrap();

    // Wait for immediate exit
    tokio::time::sleep(Duration::from_millis(600)).await;

    let status = supervisor.status("clean_exit").await.unwrap();
    assert_eq!(status, ProcessStatus::Stopped(Some(0)));
}

#[tokio::test]
async fn test_environment_variable_injection() {
    let supervisor = Supervisor::new().expect("Supervisor initialization failed");

    #[cfg(windows)]
    let env_svc = create_mock_service(
        "env_test",
        "cmd.exe",
        &[
            "/C",
            "echo PORT_VAL=%PORT% SPINMUX_VAL=%SPINMUX% SVC_VAL=%SPINMUX_SERVICE%",
        ],
    );
    #[cfg(not(windows))]
    let env_svc = create_mock_service(
        "env_test",
        "sh",
        &[
            "-c",
            "echo PORT_VAL=$PORT SPINMUX_VAL=$SPINMUX SVC_VAL=$SPINMUX_SERVICE",
        ],
    );

    let assigned_port = 45678;
    supervisor
        .add_service(env_svc, Some(assigned_port))
        .await
        .unwrap();
    supervisor.start_service("env_test").await.unwrap();

    // Await process execution and buffer capture
    tokio::time::sleep(Duration::from_millis(800)).await;

    let buffer = supervisor.get_buffer("env_test").await.unwrap();
    let snapshot = buffer.snapshot(20);

    let mut found_env_output = false;
    for entry in &snapshot {
        if entry.message.contains("PORT_VAL=45678")
            && entry.message.contains("SPINMUX_VAL=1")
            && entry.message.contains("SVC_VAL=env_test")
        {
            found_env_output = true;
            break;
        }
    }

    assert!(
        found_env_output,
        "Expected environment variables PORT=45678, SPINMUX=1, SPINMUX_SERVICE=env_test in output logs: {:?}",
        snapshot.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[tokio::test]
#[cfg(windows)]
async fn test_windows_job_object_grandchild_orphan_cleanup() {
    let supervisor = Supervisor::new().expect("Supervisor initialization failed");

    // Spawn a PowerShell process that spawns a grandchild `ping.exe` process,
    // prints its PID directly to Console stdout with flush, and waits.
    let grandchild_script = r#"$p = Start-Process -FilePath "ping" -ArgumentList "-n 60 127.0.0.1" -PassThru; [Console]::Out.WriteLine("GRANDCHILD_PID:$($p.Id)"); [Console]::Out.Flush(); $p.WaitForExit()"#;

    let svc = create_mock_service(
        "parent_spawner",
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            grandchild_script,
        ],
    );

    supervisor.add_service(svc, None).await.unwrap();
    supervisor.start_service("parent_spawner").await.unwrap();

    let buffer = supervisor.get_buffer("parent_spawner").await.unwrap();

    // Wait up to 15 seconds for grandchild PID to be emitted (accounting for CI cold-start)
    let mut grandchild_pid: Option<u32> = None;
    for _ in 0..150 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let logs = buffer.snapshot(50);
        for entry in &logs {
            if let Some(pos) = entry.message.find("GRANDCHILD_PID:") {
                let pid_str: String = entry.message[pos + "GRANDCHILD_PID:".len()..]
                    .chars()
                    .filter(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(pid) = pid_str.parse::<u32>() {
                    grandchild_pid = Some(pid);
                    break;
                }
            }
        }
        if grandchild_pid.is_some() {
            break;
        }
    }

    let gc_pid = match grandchild_pid {
        Some(pid) => pid,
        None => {
            let captured: Vec<String> = buffer
                .snapshot(100)
                .into_iter()
                .map(|e| e.message)
                .collect();
            panic!(
                "Failed to capture grandchild PID from spawner output! Logs captured: {:?}",
                captured
            );
        }
    };
    assert!(gc_pid > 0);

    // Verify grandchild process is actively running in Windows
    assert!(
        is_process_alive(gc_pid),
        "Grandchild process {} should be running in OS",
        gc_pid
    );

    // Terminate the service via Supervisor / Job Object
    supervisor.stop_service("parent_spawner").await.unwrap();

    // Verify that the grandchild process was cleanly terminated by Windows Job Objects (zero orphans)
    let mut is_dead = false;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !is_process_alive(gc_pid) {
            is_dead = true;
            break;
        }
    }

    assert!(
        is_dead,
        "CRITICAL: Grandchild process {} was NOT terminated! Windows Job Object zero-orphan enforcement failed.",
        gc_pid
    );
}

#[tokio::test]
#[cfg(not(windows))]
async fn test_windows_job_object_grandchild_orphan_cleanup() {
    // Windows Job Objects are Windows-specific; on POSIX platforms, process groups are used.
    let _supervisor = Supervisor::new().expect("Supervisor initialization failed");
}

#[tokio::test]
async fn test_supervisor_start_all_and_stop_all() {
    let supervisor = Supervisor::new().expect("Supervisor initialization failed");

    #[cfg(windows)]
    let s1 = create_mock_service("s1", "cmd.exe", &["/C", "ping -n 30 127.0.0.1 >nul"]);
    #[cfg(windows)]
    let s2 = create_mock_service("s2", "cmd.exe", &["/C", "ping -n 30 127.0.0.1 >nul"]);

    #[cfg(not(windows))]
    let s1 = create_mock_service("s1", "sleep", &["30"]);
    #[cfg(not(windows))]
    let s2 = create_mock_service("s2", "sleep", &["30"]);

    supervisor.add_service(s1, None).await.unwrap();
    supervisor.add_service(s2, None).await.unwrap();

    supervisor.start_all().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(supervisor.is_any_running().await);
    assert!(supervisor.status("s1").await.unwrap().is_running());
    assert!(supervisor.status("s2").await.unwrap().is_running());

    supervisor.stop_all().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(!supervisor.is_any_running().await);
    assert!(supervisor.status("s1").await.unwrap().is_stopped());
    assert!(supervisor.status("s2").await.unwrap().is_stopped());
}
