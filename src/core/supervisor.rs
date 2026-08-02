use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::{RwLock, mpsc, oneshot},
    time::{Duration, sleep, timeout},
};

use super::{LogBuffer, LogEntry, ManagedConfig, validate_tun_privileges};

const MAX_LOG_PENDING: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeStatus {
    pub managed: bool,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub core_url: String,
    pub config_file: PathBuf,
    pub resume_file: PathBuf,
}

enum Control {
    Start(oneshot::Sender<Result<()>>),
    Restart(oneshot::Sender<Result<()>>),
    Stop(oneshot::Sender<Result<()>>),
}

pub struct Supervisor {
    pub config: ManagedConfig,
    logs: Arc<LogBuffer>,
    status: Arc<RwLock<RuntimeStatus>>,
    control: mpsc::Sender<Control>,
}

impl Supervisor {
    pub fn new(
        binary: PathBuf,
        config: ManagedConfig,
        core_url: String,
        console_logs: bool,
    ) -> Arc<Self> {
        let logs = Arc::new(LogBuffer::default());
        let status = Arc::new(RwLock::new(RuntimeStatus {
            managed: true,
            running: false,
            pid: None,
            started_at: None,
            last_error: None,
            core_url,
            config_file: config.paths.config.clone(),
            resume_file: config.paths.resume.clone(),
        }));
        let (control, receiver) = mpsc::channel(8);
        let supervisor = Arc::new(Self {
            config: config.clone(),
            logs: logs.clone(),
            status: status.clone(),
            control,
        });
        tokio::spawn(run_manager(
            binary,
            config,
            logs,
            status,
            console_logs,
            receiver,
        ));
        supervisor
    }

    pub async fn start(&self) -> Result<()> {
        self.request(Control::Start).await
    }

    pub async fn restart(&self) -> Result<()> {
        self.request(Control::Restart).await
    }

    pub async fn stop(&self) -> Result<()> {
        self.request(Control::Stop).await?;
        self.config.restore_ownership()
    }

    async fn request(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<()>>) -> Control,
    ) -> Result<()> {
        let (sender, receiver) = oneshot::channel();
        self.control
            .send(command(sender))
            .await
            .context("Core supervisor stopped")?;
        receiver.await.context("Core supervisor stopped")?
    }

    pub async fn status(&self) -> RuntimeStatus {
        self.status.read().await.clone()
    }

    pub fn token(&self) -> Result<String> {
        self.config.token()
    }

    pub fn authorized(&self, authorization: &str) -> bool {
        self.token()
            .is_ok_and(|token| authorization == format!("Bearer {token}"))
    }

    pub fn logs_after(&self, sequence: u64, limit: usize) -> (Vec<LogEntry>, u64) {
        self.logs.after(sequence, limit)
    }
}

async fn run_manager(
    binary: PathBuf,
    config: ManagedConfig,
    logs: Arc<LogBuffer>,
    status: Arc<RwLock<RuntimeStatus>>,
    console_logs: bool,
    mut receiver: mpsc::Receiver<Control>,
) {
    let context = ManagerContext {
        binary,
        config,
        logs,
        status,
        console_logs,
    };
    let mut child: Option<Child> = None;
    let mut desired_running = false;
    let mut delayed_restart = false;
    loop {
        if desired_running && child.is_none() {
            if delayed_restart {
                tokio::select! {
                    _ = sleep(Duration::from_secs(1)) => delayed_restart = false,
                    command = receiver.recv() => {
                        if !handle_idle_command(command, &mut desired_running, &context, &mut child).await { break; }
                        delayed_restart = false;
                        continue;
                    }
                }
            }
            match start_child(&context).await {
                Ok(started) => child = Some(started),
                Err(error) => {
                    record_error(
                        &context.status,
                        &context.logs,
                        format!("Core restart failed: {error}"),
                    )
                    .await;
                    tokio::select! {
                        _ = sleep(Duration::from_secs(1)) => continue,
                        command = receiver.recv() => {
                            if !handle_idle_command(command, &mut desired_running, &context, &mut child).await { break; }
                            continue;
                        }
                    }
                }
            }
        }

        if let Some(process) = child.as_mut() {
            enum Event {
                Command(Option<Control>),
                Exited(std::io::Result<std::process::ExitStatus>),
            }
            let event = tokio::select! {
                command = receiver.recv() => Event::Command(command),
                result = process.wait() => Event::Exited(result),
            };
            match event {
                Event::Exited(result) => {
                    child = None;
                    delayed_restart = true;
                    let message = match result {
                        Ok(exit) if exit.success() => "Core exited".to_owned(),
                        Ok(exit) => format!("Core exited: {exit}"),
                        Err(error) => format!("Core exited: {error}"),
                    };
                    context.logs.append("system", &message);
                    let mut current = context.status.write().await;
                    current.running = false;
                    current.pid = None;
                    current.started_at = None;
                    if message != "Core exited" {
                        current.last_error = Some(message);
                    }
                }
                Event::Command(Some(Control::Restart(reply))) => {
                    context
                        .logs
                        .append("system", "Core restart requested by WebUI");
                    let result = terminate(process).await.map(|_| ());
                    child = None;
                    context.logs.append("system", "Core exited");
                    mark_stopped(&context.status).await;
                    if let Err(error) = result {
                        let _ = reply.send(Err(error));
                        continue;
                    }
                    match start_child(&context).await {
                        Ok(started) => {
                            child = Some(started);
                            context
                                .logs
                                .append("system", "Core restart command completed");
                            let _ = reply.send(Ok(()));
                        }
                        Err(error) => {
                            record_error(&context.status, &context.logs, error.to_string()).await;
                            let _ = reply.send(Err(error));
                        }
                    }
                }
                Event::Command(Some(Control::Stop(reply))) => {
                    desired_running = false;
                    let result = terminate(process).await;
                    child = None;
                    context.logs.append("system", "Core exited");
                    mark_stopped(&context.status).await;
                    let _ = reply.send(result);
                }
                Event::Command(Some(Control::Start(reply))) => {
                    let _ = reply.send(Ok(()));
                }
                Event::Command(None) => {
                    let _ = terminate(process).await;
                    break;
                }
            }
        } else {
            let command = receiver.recv().await;
            if !handle_idle_command(command, &mut desired_running, &context, &mut child).await {
                break;
            }
        }
    }
}

struct ManagerContext {
    binary: PathBuf,
    config: ManagedConfig,
    logs: Arc<LogBuffer>,
    status: Arc<RwLock<RuntimeStatus>>,
    console_logs: bool,
}

async fn handle_idle_command(
    command: Option<Control>,
    desired_running: &mut bool,
    context: &ManagerContext,
    child: &mut Option<Child>,
) -> bool {
    match command {
        Some(Control::Start(reply)) | Some(Control::Restart(reply)) => {
            *desired_running = true;
            let result = start_child(context).await;
            match result {
                Ok(started) => {
                    *child = Some(started);
                    let _ = reply.send(Ok(()));
                }
                Err(error) => {
                    record_error(&context.status, &context.logs, error.to_string()).await;
                    let _ = reply.send(Err(error));
                }
            }
            true
        }
        Some(Control::Stop(reply)) => {
            *desired_running = false;
            mark_stopped(&context.status).await;
            let _ = reply.send(Ok(()));
            true
        }
        None => false,
    }
}

async fn start_child(context: &ManagerContext) -> Result<Child> {
    context.config.prepare()?;
    let tun_enabled = context.config.tun_enabled()?;
    if tun_enabled {
        validate_tun_privileges()?;
    }
    let mut command = Command::new(&context.binary);
    command
        .arg("--config")
        .arg(&context.config.paths.config)
        .kill_on_drop(true);
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("start Core {}", context.binary.display()))?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow!("Core process has no PID"))?;
    if let Some(stdout) = child.stdout.take() {
        spawn_log_reader(stdout, "stdout", context.logs.clone(), context.console_logs);
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_log_reader(stderr, "stderr", context.logs.clone(), context.console_logs);
    }
    let message = if tun_enabled {
        format!("Core started with PID {pid} (privileged TUN mode)")
    } else {
        format!("Core started with PID {pid}")
    };
    context.logs.append("system", message);
    let mut current = context.status.write().await;
    current.running = true;
    current.pid = Some(pid);
    current.started_at = Some(Utc::now());
    current.last_error = None;
    Ok(child)
}

fn spawn_log_reader(
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    stream: &'static str,
    logs: Arc<LogBuffer>,
    console: bool,
) {
    tokio::spawn(async move {
        let mut reader = reader;
        let mut chunk = [0_u8; 8 * 1024];
        let mut pending = Vec::with_capacity(MAX_LOG_PENDING);
        while let Ok(read) = reader.read(&mut chunk).await {
            if read == 0 {
                break;
            }
            for byte in &chunk[..read] {
                if *byte == b'\n' {
                    append_log_line(&logs, stream, &pending, console);
                    pending.clear();
                } else {
                    pending.push(*byte);
                    if pending.len() == MAX_LOG_PENDING {
                        append_log_line(&logs, stream, &pending, console);
                        pending.clear();
                    }
                }
            }
        }
    });
}

fn append_log_line(logs: &LogBuffer, stream: &str, bytes: &[u8], console: bool) {
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    let line = String::from_utf8_lossy(bytes);
    if console {
        if stream == "stderr" {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
    logs.append(stream, line.into_owned());
}

async fn terminate(child: &mut Child) -> Result<()> {
    if let Some(pid) = child.id() {
        signal_interrupt(pid)?;
    }
    if timeout(Duration::from_secs(10), child.wait())
        .await
        .is_err()
    {
        child.start_kill().context("kill Core")?;
        child.wait().await.context("wait for killed Core")?;
    }
    Ok(())
}

#[cfg(unix)]
fn signal_interrupt(pid: u32) -> Result<()> {
    if unsafe { libc::kill(pid as i32, libc::SIGINT) } != 0 {
        return Err(std::io::Error::last_os_error()).context("signal Core");
    }
    Ok(())
}

#[cfg(not(unix))]
fn signal_interrupt(_: u32) -> Result<()> {
    Ok(())
}

async fn mark_stopped(status: &Arc<RwLock<RuntimeStatus>>) {
    let mut current = status.write().await;
    current.running = false;
    current.pid = None;
    current.started_at = None;
}

async fn record_error(status: &Arc<RwLock<RuntimeStatus>>, logs: &Arc<LogBuffer>, message: String) {
    logs.append("system", &message);
    let mut current = status.write().await;
    current.running = false;
    current.pid = None;
    current.started_at = None;
    current.last_error = Some(message);
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::core::ManagedPaths;

    fn fixture(script: &str, name: &str) -> (PathBuf, ManagedConfig, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "sumire-supervisor-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let binary = root.join("fake-core");
        std::fs::write(&binary, script).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let paths = ManagedPaths::resolve(&root.join("data"), None, None).unwrap();
        let config = ManagedConfig {
            paths,
            core_listen: "127.0.0.1:19090".into(),
        };
        (binary, config, root)
    }

    #[tokio::test]
    async fn restart_replaces_managed_process() {
        let (binary, config, root) = fixture(
            "#!/bin/sh\ntrap 'exit 0' INT TERM\necho ready\nwhile :; do sleep 1; done\n",
            "restart",
        );
        let supervisor = Supervisor::new(binary, config, "http://127.0.0.1:19090".into(), false);
        supervisor.start().await.unwrap();
        let first = supervisor.status().await.pid.unwrap();
        supervisor.restart().await.unwrap();
        let second = supervisor.status().await.pid.unwrap();
        assert_ne!(first, second);
        supervisor.stop().await.unwrap();
        assert!(!supervisor.status().await.running);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn unexpected_exit_is_restarted() {
        let (binary, config, root) = fixture(
            "#!/bin/sh\ncount_file=\"$0.count\"\ncount=0\n[ -f \"$count_file\" ] && count=$(cat \"$count_file\")\ncount=$((count + 1))\necho \"$count\" > \"$count_file\"\n[ \"$count\" -eq 1 ] && exit 7\ntrap 'exit 0' INT TERM\nwhile :; do sleep 1; done\n",
            "recover",
        );
        let supervisor = Supervisor::new(binary, config, "http://127.0.0.1:19090".into(), false);
        let started = std::time::Instant::now();
        supervisor.start().await.unwrap();
        let first = supervisor.status().await.pid;
        let mut restarted = false;
        for _ in 0..100 {
            let status = supervisor.status().await;
            if status.running && status.pid.is_some() && status.pid != first {
                restarted = true;
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
        assert!(restarted, "Core was not restarted after unexpected exit");
        assert!(started.elapsed() >= Duration::from_millis(900));
        let (entries, _) = supervisor.logs_after(0, 500);
        assert!(
            entries
                .iter()
                .any(|entry| entry.message.starts_with("Core exited"))
        );
        supervisor.stop().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
