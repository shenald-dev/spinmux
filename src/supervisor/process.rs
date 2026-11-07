use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{watch, Mutex, RwLock};

use super::job_object::WinJob;
use super::posix_fallback::configure_posix_command;
use crate::discovery::ServiceDefinition;
use crate::ringbuf::{spawn_stream_reader, LogEntry, LogStream, RingBuffer};

/// Allocates an ephemeral OS port by binding to 127.0.0.1:0
pub fn allocate_ephemeral_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Lifecycle execution status of a supervised process
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessStatus {
    /// Initializing or preparing to launch
    Starting,
    /// Actively executing with assigned PID
    Running(u32),
    /// Process has terminated with optional exit code
    Stopped(Option<i32>),
    /// Process terminated unexpectedly; waiting out restart backoff in milliseconds
    Crashing(u64),
    /// Service crashed repeatedly and automatic restarts are halted
    CrashLoopBackOff,
}

impl ProcessStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, ProcessStatus::Running(_))
    }

    pub fn is_stopped(&self) -> bool {
        matches!(self, ProcessStatus::Stopped(_))
    }

    pub fn pid(&self) -> Option<u32> {
        match self {
            ProcessStatus::Running(pid) => Some(*pid),
            _ => None,
        }
    }

    pub fn exit_code(&self) -> Option<i32> {
        match self {
            ProcessStatus::Stopped(code) => *code,
            _ => None,
        }
    }
}

impl std::fmt::Display for ProcessStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessStatus::Starting => write!(f, "Starting"),
            ProcessStatus::Running(pid) => write!(f, "Running (PID: {})", pid),
            ProcessStatus::Stopped(Some(code)) => write!(f, "Stopped (exit code: {})", code),
            ProcessStatus::Stopped(None) => write!(f, "Stopped"),
            ProcessStatus::Crashing(backoff) => write!(f, "Crashing (backoff: {}ms)", backoff),
            ProcessStatus::CrashLoopBackOff => write!(f, "CrashLoopBackOff"),
        }
    }
}

/// Errors occurring during process lifecycle supervision
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("Failed to spawn process '{0}': {1}")]
    SpawnFailed(String, std::io::Error),

    #[error("Failed to create Windows Job Object: {0}")]
    JobCreation(std::io::Error),

    #[error("Failed to assign process {0} to Windows Job Object: {1}")]
    JobAssignment(u32, std::io::Error),

    #[error("Service not found: '{0}'")]
    ServiceNotFound(String),

    #[error("Service '{0}' is already running with PID {1}")]
    AlreadyRunning(String, u32),

    #[error("Timeout while stopping service '{0}'")]
    StopTimeout(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Internal state for a supervised service
pub struct SupervisedService {
    pub definition: ServiceDefinition,
    pub assigned_port: u16,
    pub status_tx: watch::Sender<ProcessStatus>,
    pub status_rx: watch::Receiver<ProcessStatus>,
    pub buffer: Arc<RingBuffer>,
    pub job: Arc<WinJob>,
    pub current_pid: Arc<AtomicU32>,
    pub stop_signal_tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    pub auto_restart: Arc<AtomicBool>,
    pub crash_count: Arc<AtomicU32>,
    pub last_crash: Arc<Mutex<Option<Instant>>>,
    pub started_at: Arc<Mutex<Option<Instant>>>,
}

impl SupervisedService {
    pub fn new(definition: ServiceDefinition, assigned_port: u16, job: Arc<WinJob>) -> Self {
        let (status_tx, status_rx) = watch::channel(ProcessStatus::Stopped(None));
        Self {
            definition,
            assigned_port,
            status_tx,
            status_rx,
            buffer: Arc::new(RingBuffer::default()),
            job,
            current_pid: Arc::new(AtomicU32::new(0)),
            stop_signal_tx: Arc::new(Mutex::new(None)),
            auto_restart: Arc::new(AtomicBool::new(true)),
            crash_count: Arc::new(AtomicU32::new(0)),
            last_crash: Arc::new(Mutex::new(None)),
            started_at: Arc::new(Mutex::new(None)),
        }
    }

    pub fn status(&self) -> ProcessStatus {
        self.status_rx.borrow().clone()
    }
}

/// Process Supervisor Engine managing multiple services
#[derive(Clone)]
pub struct Supervisor {
    services: Arc<RwLock<HashMap<String, Arc<SupervisedService>>>>,
    master_job: Arc<WinJob>,
}

impl Supervisor {
    /// Creates a new Supervisor engine with a master Windows Job Object
    pub fn new() -> Result<Self, SupervisorError> {
        let master_job = Arc::new(WinJob::new().map_err(SupervisorError::JobCreation)?);
        Ok(Self {
            services: Arc::new(RwLock::new(HashMap::new())),
            master_job,
        })
    }

    /// Returns reference to master Windows Job Object
    pub fn master_job(&self) -> Arc<WinJob> {
        self.master_job.clone()
    }

    /// Registers a service definition, allocating a port if not statically defined
    pub async fn add_service(
        &self,
        definition: ServiceDefinition,
        port_override: Option<u16>,
    ) -> Result<Arc<SupervisedService>, SupervisorError> {
        let port = if let Some(p) = port_override {
            p
        } else if let Some(p) = definition.target_port {
            p
        } else {
            allocate_ephemeral_port().map_err(SupervisorError::Io)?
        };

        // Create dedicated Windows Job Object for this service
        let service_job = Arc::new(WinJob::new().map_err(SupervisorError::JobCreation)?);
        let service = Arc::new(SupervisedService::new(
            definition.clone(),
            port,
            service_job,
        ));

        let mut map = self.services.write().await;
        map.insert(definition.id.clone(), service.clone());
        if definition.id != definition.name {
            map.insert(definition.name.clone(), service.clone());
        }
        if !definition.subdomain.is_empty() {
            map.insert(definition.subdomain.clone(), service.clone());
        }

        Ok(service)
    }

    /// Retrieves a supervised service by ID or name
    pub async fn get_service(&self, service_id: &str) -> Option<Arc<SupervisedService>> {
        let map = self.services.read().await;
        map.get(service_id).cloned()
    }

    /// Retrieves a supervised service by its virtual localhost subdomain
    pub async fn get_service_by_subdomain(
        &self,
        subdomain: &str,
    ) -> Option<Arc<SupervisedService>> {
        let map = self.services.read().await;
        for svc in map.values() {
            if svc.definition.subdomain.eq_ignore_ascii_case(subdomain) {
                return Some(svc.clone());
            }
        }
        for svc in map.values() {
            if svc.definition.id.eq_ignore_ascii_case(subdomain)
                || svc.definition.name.eq_ignore_ascii_case(subdomain)
            {
                return Some(svc.clone());
            }
        }
        None
    }

    /// Returns a list of all uniquely registered services
    pub async fn all_services(&self) -> Vec<Arc<SupervisedService>> {
        let map = self.services.read().await;
        let mut unique = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for svc in map.values() {
            if seen.insert(svc.definition.id.clone()) {
                unique.push(svc.clone());
            }
        }
        unique
    }

    /// Returns the designated default service (web/frontend or first HTTP service)
    pub async fn get_default_service(&self) -> Option<Arc<SupervisedService>> {
        let services = self.all_services().await;
        for svc in &services {
            if svc.definition.subdomain.eq_ignore_ascii_case("web")
                || svc.definition.subdomain.eq_ignore_ascii_case("frontend")
                || svc.definition.name.eq_ignore_ascii_case("web")
                || svc.definition.name.eq_ignore_ascii_case("frontend")
            {
                return Some(svc.clone());
            }
        }
        for svc in &services {
            if svc.definition.is_http {
                return Some(svc.clone());
            }
        }
        services.into_iter().next()
    }

    /// Retrieves the log ring-buffer for a service
    pub async fn get_buffer(&self, service_id: &str) -> Option<Arc<RingBuffer>> {
        self.get_service(service_id).await.map(|s| s.buffer.clone())
    }

    /// Returns current lifecycle status of a service
    pub async fn status(&self, service_id: &str) -> Option<ProcessStatus> {
        self.get_service(service_id).await.map(|s| s.status())
    }

    /// Returns current statuses for all registered unique services
    pub async fn all_statuses(&self) -> HashMap<String, ProcessStatus> {
        let map = self.services.read().await;
        let mut out = HashMap::new();
        for (k, svc) in map.iter() {
            if k == &svc.definition.id {
                out.insert(svc.definition.id.clone(), svc.status());
            }
        }
        out
    }

    /// Spawns a supervised service asynchronously
    pub async fn start_service(&self, service_id: &str) -> Result<(), SupervisorError> {
        let service = self
            .get_service(service_id)
            .await
            .ok_or_else(|| SupervisorError::ServiceNotFound(service_id.to_string()))?;

        service.auto_restart.store(true, Ordering::SeqCst);
        start_supervised_child(service).await
    }

    /// Gracefully stops a running service process
    pub async fn stop_service(&self, service_id: &str) -> Result<(), SupervisorError> {
        let service = self
            .get_service(service_id)
            .await
            .ok_or_else(|| SupervisorError::ServiceNotFound(service_id.to_string()))?;

        stop_supervised_child(service, Duration::from_millis(2000)).await
    }

    /// Restarts a service process, resetting crash backoff
    pub async fn restart_service(&self, service_id: &str) -> Result<(), SupervisorError> {
        let service = self
            .get_service(service_id)
            .await
            .ok_or_else(|| SupervisorError::ServiceNotFound(service_id.to_string()))?;

        // 1. Stop if currently running
        if service.status().is_running() {
            let _ = stop_supervised_child(service.clone(), Duration::from_millis(2000)).await;
        }

        // 2. Reset crash counters
        service.crash_count.store(0, Ordering::SeqCst);
        let mut last_crash_guard = service.last_crash.lock().await;
        *last_crash_guard = None;
        drop(last_crash_guard);

        service.auto_restart.store(true, Ordering::SeqCst);
        start_supervised_child(service).await
    }

    /// Starts all registered services
    pub async fn start_all(&self) -> Result<(), SupervisorError> {
        let map = self.services.read().await;
        let mut unique = Vec::new();
        for (k, svc) in map.iter() {
            if k == &svc.definition.id {
                unique.push(svc.clone());
            }
        }
        drop(map);

        for svc in unique {
            if !svc.status().is_running() {
                svc.auto_restart.store(true, Ordering::SeqCst);
                let _ = start_supervised_child(svc).await;
            }
        }
        Ok(())
    }

    /// Stops all registered services and terminates all Job Objects
    pub async fn stop_all(&self) -> Result<(), SupervisorError> {
        let map = self.services.read().await;
        let mut unique = Vec::new();
        for (k, svc) in map.iter() {
            if k == &svc.definition.id {
                unique.push(svc.clone());
            }
        }
        drop(map);

        for svc in unique {
            let _ = stop_supervised_child(svc, Duration::from_millis(1000)).await;
        }

        // Final safety net: terminate master job
        let _ = self.master_job.terminate(0);
        Ok(())
    }

    /// Checks if any service is actively running
    pub async fn is_any_running(&self) -> bool {
        let map = self.services.read().await;
        map.values().any(|s| s.status().is_running())
    }
}

/// Internal helper to launch a child process and attach supervisor monitoring
fn start_supervised_child(
    service: Arc<SupervisedService>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SupervisorError>> + Send>> {
    Box::pin(async move {
        let current_status = service.status();
        if let ProcessStatus::Running(pid) = current_status {
            return Err(SupervisorError::AlreadyRunning(
                service.definition.name.clone(),
                pid,
            ));
        }

        let _ = service.status_tx.send(ProcessStatus::Starting);
        service.buffer.push(LogEntry::new(
            &service.definition.name,
            LogStream::System,
            format!("Starting service '{}'...", service.definition.name),
        ));

        let mut cmd = tokio::process::Command::new(&service.definition.command);
        cmd.args(&service.definition.args);

        if service.definition.working_dir.exists() {
            cmd.current_dir(&service.definition.working_dir);
        }

        // Dynamic environment variables injection
        for (k, v) in &service.definition.env {
            cmd.env(k, v);
        }
        cmd.env("PORT", service.assigned_port.to_string());
        cmd.env("SPINMUX", "1");
        cmd.env("SPINMUX_SERVICE", &service.definition.name);
        cmd.env("SPINMUX_PORT", service.assigned_port.to_string());

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.stdin(std::process::Stdio::null());

        configure_posix_command(&mut cmd);

        let mut child = cmd.spawn().map_err(|e| {
            let _ = service.status_tx.send(ProcessStatus::Stopped(None));
            SupervisorError::SpawnFailed(service.definition.name.clone(), e)
        })?;

        let pid = child.id().unwrap_or(0);

        // Assign to Windows Job Object immediately
        #[cfg(windows)]
        {
            let assign_err = {
                if let Some(raw_handle) = child.raw_handle() {
                    service.job.assign_process(raw_handle).err()
                } else {
                    None
                }
            };

            if let Some(e) = assign_err {
                let is_already_dead = child.try_wait().ok().flatten().is_some();
                if !is_already_dead {
                    let _ = child.start_kill();
                    let _ = service.status_tx.send(ProcessStatus::Stopped(None));
                    return Err(SupervisorError::JobAssignment(pid, e));
                }
            }
        }

        service.current_pid.store(pid, Ordering::SeqCst);
        let _ = service.status_tx.send(ProcessStatus::Running(pid));
        {
            let mut started_guard = service.started_at.lock().await;
            *started_guard = Some(Instant::now());
        }

        service.buffer.push(LogEntry::new(
            &service.definition.name,
            LogStream::System,
            format!(
                "Service '{}' running with PID {} [port: {}]",
                service.definition.name, pid, service.assigned_port
            ),
        ));

        // Asynchronous stdout & stderr streaming into RingBuffer
        if let Some(stdout) = child.stdout.take() {
            spawn_stream_reader(
                stdout,
                service.definition.name.clone(),
                LogStream::Stdout,
                service.buffer.clone(),
            );
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_stream_reader(
                stderr,
                service.definition.name.clone(),
                LogStream::Stderr,
                service.buffer.clone(),
            );
        }

        // Oneshot channel to signal graceful stop
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        {
            let mut stop_guard = service.stop_signal_tx.lock().await;
            *stop_guard = Some(stop_tx);
        }

        let svc_clone = service.clone();
        tokio::spawn(async move {
            let svc_name = svc_clone.definition.name.clone();

            tokio::select! {
                _ = &mut stop_rx => {
                    // Graceful stop requested
                    let grace_timeout = tokio::time::sleep(Duration::from_millis(1500));
                    tokio::pin!(grace_timeout);

                    tokio::select! {
                        exit_res = child.wait() => {
                            let code = exit_res.ok().and_then(|s| s.code());
                            let _ = svc_clone.status_tx.send(ProcessStatus::Stopped(code));
                        }
                        _ = &mut grace_timeout => {
                            // Force terminate via Job Object / kill
                            let _ = svc_clone.job.terminate(1);
                            let _ = child.kill().await;
                            let _ = svc_clone.status_tx.send(ProcessStatus::Stopped(Some(1)));
                        }
                    }
                    svc_clone.current_pid.store(0, Ordering::SeqCst);
                    svc_clone.buffer.push(LogEntry::new(
                        &svc_name,
                        LogStream::System,
                        format!("Service '{}' stopped", svc_name),
                    ));
                }
                exit_res = child.wait() => {
                    let code = exit_res.ok().and_then(|s| s.code());
                    svc_clone.current_pid.store(0, Ordering::SeqCst);

                    let is_clean_exit = code == Some(0);
                    let should_restart = svc_clone.auto_restart.load(Ordering::SeqCst);

                    if is_clean_exit || !should_restart {
                        let _ = svc_clone.status_tx.send(ProcessStatus::Stopped(code));
                        svc_clone.buffer.push(LogEntry::new(
                            &svc_name,
                            LogStream::System,
                            format!("Service '{}' exited with code {:?}", svc_name, code),
                        ));
                    } else {
                        handle_service_crash(svc_clone, code).await;
                    }
                }
            }
        });

        Ok(())
    })
}

/// Handles unexpected service process crashes with exponential backoff
async fn handle_service_crash(service: Arc<SupervisedService>, exit_code: Option<i32>) {
    let service_name = service.definition.name.clone();
    let now = Instant::now();

    // Check if the service had been running stably for >= 30 seconds
    {
        let started_guard = service.started_at.lock().await;
        if let Some(started) = *started_guard {
            if now.duration_since(started).as_secs() >= 30 {
                service.crash_count.store(0, Ordering::SeqCst);
            }
        }
    }

    let failures = service.crash_count.fetch_add(1, Ordering::SeqCst) + 1;

    // Check rapid crash frequency
    {
        let mut last_crash_guard = service.last_crash.lock().await;
        *last_crash_guard = Some(now);
    }

    // CrashLoopBackOff if 5 rapid failures occurred
    if failures >= 5 {
        let _ = service.status_tx.send(ProcessStatus::CrashLoopBackOff);
        service.buffer.push(LogEntry::new(
            &service_name,
            LogStream::System,
            format!(
                "Service '{}' entered CrashLoopBackOff after {} consecutive failures",
                service_name, failures
            ),
        ));
        return;
    }

    // Exponential backoff: 500ms * 2^(failures - 1), capped at 10,000ms
    let exponent = (failures.saturating_sub(1)).min(5);
    let backoff_ms = (500u64 * (1u64 << exponent)).min(10_000);

    let _ = service.status_tx.send(ProcessStatus::Crashing(backoff_ms));
    service.buffer.push(LogEntry::new(
        &service_name,
        LogStream::System,
        format!(
            "Service '{}' crashed (exit code {:?}). Restarting in {}ms (failure {}/5)...",
            service_name, exit_code, backoff_ms, failures
        ),
    ));

    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;

    if service.auto_restart.load(Ordering::SeqCst) {
        let svc_to_restart = service.clone();
        tokio::spawn(async move {
            let _ = start_supervised_child(svc_to_restart).await;
        });
    }
}

/// Internal helper to stop a supervised child process
async fn stop_supervised_child(
    service: Arc<SupervisedService>,
    timeout: Duration,
) -> Result<(), SupervisorError> {
    service.auto_restart.store(false, Ordering::SeqCst);

    if !service.status().is_running() {
        return Ok(());
    }

    let mut stop_guard = service.stop_signal_tx.lock().await;
    if let Some(tx) = stop_guard.take() {
        let _ = tx.send(());
    }
    drop(stop_guard);

    // Wait for process to exit or reach timeout
    let mut rx = service.status_rx.clone();
    let wait_res = tokio::time::timeout(timeout, async {
        while rx.borrow().is_running() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    })
    .await;

    if wait_res.is_err() {
        // Force terminate via Job Object
        let _ = service.job.terminate(1);
        service.current_pid.store(0, Ordering::SeqCst);
        let _ = service.status_tx.send(ProcessStatus::Stopped(Some(1)));
    }

    Ok(())
}
