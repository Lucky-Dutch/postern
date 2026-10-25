//! SSH local port forwarding through the system OpenSSH client.
//!
//! OpenSSH handles host-key verification (`~/.ssh/known_hosts`), the SSH agent,
//! identity files, and `~/.ssh/config`. `BatchMode=yes` prevents interactive
//! prompts, so a tunnel never asks for or receives a password. The child process
//! is killed when the tunnel is dropped.

use crate::models::SshTunnelConfig;
use anyhow::{Context, Result, bail, ensure};
use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, process::Child};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

pub struct SshTunnel {
    child: Child,
    pub local_port: u16,
    config: SshTunnelConfig,
    target: (String, u16),
}

impl SshTunnel {
    /// Starts `ssh -N -L` and waits until the forwarded local port accepts connections.
    pub async fn open(
        config: &SshTunnelConfig,
        target_host: &str,
        target_port: u16,
    ) -> Result<Self> {
        config.validate()?;
        let target_host = target_host.trim();
        ensure!(
            !target_host.starts_with('/'),
            "An SSH tunnel needs a TCP database host, not a Unix socket directory."
        );
        ensure!(
            !target_host.starts_with('-') && !target_host.contains(['\0', '\n', '\r']),
            "The database host is not valid for an SSH tunnel."
        );
        let local_port = {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
                .context("Could not reserve a local port for the SSH tunnel.")?;
            listener.local_addr()?.port()
        };
        let forwarded_host = if target_host.contains(':') {
            format!("[{target_host}]")
        } else {
            target_host.to_owned()
        };
        let mut command = tokio::process::Command::new("ssh");
        command
            .args(["-N", "-T"])
            .args(["-o", "BatchMode=yes"])
            .args(["-o", "ExitOnForwardFailure=yes"])
            .args(["-o", "ServerAliveInterval=30"])
            .args(["-o", "ConnectTimeout=15"])
            .arg("-o")
            .arg(format!(
                "StrictHostKeyChecking={}",
                if config.accept_new_host_key {
                    "accept-new"
                } else {
                    "yes"
                }
            ))
            .arg("-L")
            .arg(format!(
                "127.0.0.1:{local_port}:{forwarded_host}:{target_port}"
            ))
            .arg("-p")
            .arg(config.port.to_string());
        let identity = config.identity_file.trim();
        if !identity.is_empty() {
            command
                .arg("-i")
                .arg(expand_home(identity))
                .args(["-o", "IdentitiesOnly=yes"]);
        }
        let user = config.user.trim();
        if !user.is_empty() {
            command.arg("-l").arg(user);
        }
        command
            .arg("--")
            .arg(config.host.trim())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .context("Could not start the ssh command. Install OpenSSH or check PATH.")?;
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(mut pipe) = child.stderr.take() {
            let stderr = stderr.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 1024];
                while let Ok(read) = pipe.read(&mut buffer).await {
                    if read == 0 {
                        break;
                    }
                    let mut text = stderr.lock().expect("stderr buffer");
                    if text.len() < 8192 {
                        text.push_str(&String::from_utf8_lossy(&buffer[..read]));
                    }
                }
            });
        }
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let output = stderr.lock().expect("stderr buffer").trim().to_owned();
                bail!(
                    "The SSH tunnel stopped ({status}).{}{}",
                    if output.is_empty() { "" } else { "\n" },
                    output
                );
            }
            if tokio::net::TcpStream::connect(("127.0.0.1", local_port))
                .await
                .is_ok()
            {
                break;
            }
            if started.elapsed() > STARTUP_TIMEOUT {
                let _ = child.kill().await;
                bail!(
                    "The SSH tunnel did not start within {} seconds.",
                    STARTUP_TIMEOUT.as_secs()
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(Self {
            child,
            local_port,
            config: config.clone(),
            target: (target_host.to_owned(), target_port),
        })
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// True when this tunnel forwards to the same server through the same SSH host.
    pub fn serves(&self, config: &SshTunnelConfig, target_host: &str, target_port: u16) -> bool {
        &self.config == config && self.target == (target_host.trim().to_owned(), target_port)
    }
}

fn expand_home(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(dirs) = directories::BaseDirs::new()
    {
        return dirs.home_dir().join(rest);
    }
    std::path::PathBuf::from(path)
}
