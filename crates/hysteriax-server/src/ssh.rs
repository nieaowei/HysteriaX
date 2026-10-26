use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use russh::{
    ChannelMsg,
    client::{self, Handle},
    keys::{HashAlg, PublicKeyOrCertificate},
};
use russh_sftp::client::SftpSession;
use thiserror::Error;
use tokio::io::AsyncWriteExt;

#[derive(Clone, Debug)]
pub struct SshNode {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_type: String,
    pub secret: String,
    pub passphrase: Option<String>,
    pub host_fingerprint: Option<String>,
}

#[derive(Debug, Error)]
pub enum SshError {
    #[error("SSH authentication failed; check the username and credential")]
    AuthenticationFailed,
    #[error("{remaining} client device(s) remain online after repeated Hysteria kick requests")]
    ClientsStillOnline { remaining: u64 },
    #[error("SSH connection failed: {0}")]
    Transport(String),
    #[error("SSH operation exceeded its execution deadline; remote state requires verification")]
    OperationTimedOut,
    #[error("SSH response exceeded its output limit; remote state requires verification")]
    OutputLimitExceeded,
    #[error("remote command failed with exit code {code}: {stderr}")]
    Command { code: u32, stderr: String },
}

pub(crate) mod journal;

pub struct SshSession {
    handle: Handle<HostKeyHandler>,
    // The library spawns its protocol loop independently of connect_stream.
    // Closing the socket also wakes a loop still waiting for key exchange.
    _socket: Option<SocketGuard>,
    pub fingerprint: String,
    pub(crate) endpoint: journal::Endpoint,
}

struct SocketGuard(std::net::TcpStream);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

async fn connect_transport(
    config: Arc<client::Config>,
    address: (&str, u16),
    handler: HostKeyHandler,
) -> Result<(Handle<HostKeyHandler>, SocketGuard)> {
    let socket = tokio::net::TcpStream::connect(address).await?.into_std()?;
    let guard = SocketGuard(socket.try_clone()?);
    let stream = tokio::net::TcpStream::from_std(socket)?;
    if config.nodelay {
        stream.set_nodelay(true)?;
    }
    let handle = client::connect_stream(config, stream, handler).await?;
    Ok((handle, guard))
}

#[derive(Clone, Debug)]
pub struct CommandOutput {
    pub code: u32,
    pub stdout: String,
    pub stderr: String,
}

pub enum FingerprintResult {
    Trusted(SshSession),
    NeedsConfirmation { fingerprint: String },
    Changed { expected: String, observed: String },
}

#[derive(Clone)]
struct HostKeyHandler {
    expected: Option<String>,
    observed: Arc<Mutex<Option<String>>>,
}

impl client::Handler for HostKeyHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let fingerprint = server_public_key
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string();
        if let Ok(mut observed) = self.observed.lock() {
            *observed = Some(fingerprint.clone());
        }
        Ok(self.expected.as_deref() == Some(fingerprint.as_str()))
    }
}

pub async fn connect(node: &SshNode) -> Result<FingerprintResult> {
    crate::execution::check_current().await?;
    let observed = Arc::new(Mutex::new(None));
    let handler = HostKeyHandler {
        expected: node.host_fingerprint.clone(),
        observed: observed.clone(),
    };
    let config = client::Config {
        inactivity_timeout: Some(Duration::from_secs(60)),
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 3,
        ..Default::default()
    };
    let result = match tokio::time::timeout(
        Duration::from_secs(crate::runtime_config::operation_timeout_seconds(15)),
        connect_transport(Arc::new(config), (node.host.as_str(), node.port), handler),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => return Err(SshError::Transport("SSH connection timed out".to_owned()).into()),
    };

    let (mut handle, socket) = match result {
        Ok(connection) => connection,
        Err(error) => {
            let fingerprint = observed.lock().ok().and_then(|value| value.clone());
            if let Some(observed) = fingerprint {
                return match &node.host_fingerprint {
                    None => Ok(FingerprintResult::NeedsConfirmation {
                        fingerprint: observed,
                    }),
                    Some(expected) if expected != &observed => Ok(FingerprintResult::Changed {
                        expected: expected.clone(),
                        observed,
                    }),
                    Some(_) => Err(SshError::Transport(error.to_string()).into()),
                };
            }
            return Err(SshError::Transport(error.to_string()).into());
        }
    };

    let fingerprint = observed
        .lock()
        .ok()
        .and_then(|value| value.clone())
        .context("SSH server did not provide a host key")?;
    if node.host_fingerprint.is_none() {
        return Ok(FingerprintResult::NeedsConfirmation { fingerprint });
    }

    let auth = if node.auth_type == "password" {
        tokio::time::timeout(
            Duration::from_secs(crate::runtime_config::operation_timeout_seconds(15)),
            handle.authenticate_password(node.username.clone(), node.secret.clone()),
        )
        .await
        .map_err(|_| SshError::Transport("SSH password authentication timed out".to_owned()))?
    } else {
        let key = russh::keys::decode_secret_key(&node.secret, node.passphrase.as_deref())
            .context("decode SSH private key; check its format and passphrase")?;
        let hash = tokio::time::timeout(
            Duration::from_secs(crate::runtime_config::operation_timeout_seconds(15)),
            handle.best_supported_rsa_hash(),
        )
        .await
        .map_err(|_| SshError::Transport("SSH signature negotiation timed out".to_owned()))?
        .map_err(|error| SshError::Transport(error.to_string()))?
        .flatten();
        let key = russh::keys::key::PrivateKeyWithHashAlg::new(Arc::new(key), hash);
        tokio::time::timeout(
            Duration::from_secs(crate::runtime_config::operation_timeout_seconds(15)),
            handle.authenticate_publickey(node.username.clone(), key),
        )
        .await
        .map_err(|_| SshError::Transport("SSH public-key authentication timed out".to_owned()))?
    }
    .map_err(|error| SshError::Transport(error.to_string()))?;
    if !auth.success() {
        return Err(SshError::AuthenticationFailed.into());
    }
    Ok(FingerprintResult::Trusted(SshSession {
        handle,
        _socket: Some(socket),
        endpoint: journal::Endpoint {
            host: node.host.clone(),
            port: node.port,
            username: node.username.clone(),
            fingerprint: fingerprint.clone(),
        },
        fingerprint,
    }))
}

impl SshSession {
    pub async fn execute(&self, command: &str) -> Result<CommandOutput> {
        self.execute_with_limits(
            command,
            Duration::from_secs(crate::runtime_config::operation_timeout_seconds(60)),
            4 * 1024 * 1024,
        )
        .await
    }

    /// Only for fixed observation commands that cannot change the remote node.
    /// These reads may be retried after ownership loss; mutations use execute.
    pub async fn execute_read_only(&self, command: &str) -> Result<CommandOutput> {
        crate::execution::check_current().await?;
        self.execute_remote(
            command,
            Duration::from_secs(crate::runtime_config::operation_timeout_seconds(60)),
            4 * 1024 * 1024,
        )
        .await
    }
    pub async fn execute_read_only_checked(&self, command: &str) -> Result<String> {
        Self::check_output(self.execute_read_only(command).await?)
    }
    async fn execute_with_limits(
        &self,
        command: &str,
        timeout: Duration,
        max_output: usize,
    ) -> Result<CommandOutput> {
        let prepared_head = if let Some(lease) =
            crate::execution::remote::resource_for("ssh-command")?
        {
            let observed = async {
                let text = self
                    .execute_read_only_checked(&journal::head(&lease.resource)?)
                    .await?;
                let head = journal::parse_head(&text)?;
                crate::execution::check_current().await?;
                Ok::<_, anyhow::Error>(head)
            }
            .await;
            match observed {
                Ok(head) => Some(head),
                Err(error) => {
                    // No requested payload is sent when its remote predecessor
                    // is unreadable. Persist the resulting safety barrier.
                    return crate::execution::remote::with_context(
                        "ssh-command",crate::security::token_digest(command),
                        serde_json::json!({"protocol":"ssh-journal-v2","endpoint":self.endpoint,"max_output":max_output,"preparation":"unverified"}),
                        move |_|async move {Err(error)},|_:&CommandOutput|serde_json::Value::Null,
                    ).await;
                }
            }
        } else {
            None
        };
        crate::execution::remote::with_context(
            "ssh-command",
            crate::security::token_digest(command),
            serde_json::json!({"protocol":"ssh-journal-v2","endpoint":self.endpoint,"max_output":max_output,"expected_head":prepared_head}),
            |context| async move {
                let Some(context)=context else {return self.execute_remote(command,timeout,max_output).await;};
                let wrapped=journal::wrap(&context,command,max_output)?;
                let output=self.execute_remote(&wrapped,timeout,max_output).await?;
                let receipt=self.inspect_command(&context,&self.endpoint,max_output).await?;
                anyhow::ensure!(receipt.exit_code==output.code,"remote command receipt differs from its acknowledgement");
                Ok(output)
            },
            |output| serde_json::json!({"exit_code":output.code,"stdout_digest":crate::security::token_digest(&output.stdout),"stderr_digest":crate::security::token_digest(&output.stderr)}),
        ).await
    }

    /// Read only a completed original-command receipt, with exact endpoint binding.
    pub(crate) async fn inspect_command(
        &self,
        step: &crate::execution::remote::RemoteStep,
        endpoint: &journal::Endpoint,
        max_output: usize,
    ) -> Result<journal::Receipt> {
        anyhow::ensure!(
            &self.endpoint == endpoint,
            "remote verification endpoint changed"
        );
        let output = self
            .execute_remote(
                &journal::inspect(step)?,
                Duration::from_secs(crate::runtime_config::operation_timeout_seconds(10)),
                journal::RECEIPT_LIMIT,
            )
            .await?;
        let text = Self::check_output(output)?;
        journal::parse(text.as_bytes(), step, max_output)
    }

    pub(crate) async fn fence_command(
        &self,
        request: &journal::recheck::Request,
        endpoint: &journal::Endpoint,
    ) -> Result<journal::recheck::Proof> {
        anyhow::ensure!(
            &self.endpoint == endpoint,
            "remote verification endpoint changed"
        );
        crate::execution::check_current().await?;
        let output = self
            .execute_remote(
                &journal::recheck::render(request)?,
                Duration::from_secs(crate::runtime_config::operation_timeout_seconds(15)),
                journal::RECEIPT_LIMIT,
            )
            .await?;
        let text = Self::check_output(output)?;
        journal::recheck::parse(text.as_bytes(), request)
    }

    async fn execute_remote(
        &self,
        command: &str,
        timeout: Duration,
        max_output: usize,
    ) -> Result<CommandOutput> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut channel = tokio::time::timeout_at(deadline, self.handle.channel_open_session())
            .await
            .map_err(|_| SshError::OperationTimedOut)??;
        let result = tokio::time::timeout_at(deadline, async {
            channel.exec(true, command).await?;
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let mut exit_code = None;
            while let Some(message) = channel.wait().await {
                match message {
                    ChannelMsg::Data { data } => {
                        if data.len() > max_output.saturating_sub(stdout.len() + stderr.len()) {
                            return Err(SshError::OutputLimitExceeded.into());
                        }
                        stdout.extend_from_slice(&data);
                    }
                    ChannelMsg::ExtendedData { data, ext: 1 } => {
                        if data.len() > max_output.saturating_sub(stdout.len() + stderr.len()) {
                            return Err(SshError::OutputLimitExceeded.into());
                        }
                        stderr.extend_from_slice(&data);
                    }
                    ChannelMsg::ExitStatus { exit_status } => exit_code = Some(exit_status),
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
            Ok(CommandOutput {
                code: exit_code.context(
                    "SSH command completion was not acknowledged; verification required",
                )?,
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            })
        })
        .await
        .unwrap_or_else(|_| Err(SshError::OperationTimedOut.into()));
        if result.is_err() {
            // SSH signals are best effort. Never classify uncertain execution as
            // a retryable transport error: a peer may ignore TERM/close.
            let _ = tokio::time::timeout(Duration::from_secs(1), async {
                let _ = channel.signal(russh::Sig::TERM).await;
                let _ = channel.close().await;
            })
            .await;
        }
        result
    }

    pub async fn execute_checked_with_timeout(
        &self,
        command: &str,
        timeout: Duration,
    ) -> Result<String> {
        Self::check_output(
            self.execute_with_limits(command, timeout, 4 * 1024 * 1024)
                .await?,
        )
    }

    pub async fn execute_checked(&self, command: &str) -> Result<String> {
        Self::check_output(self.execute(command).await?)
    }

    fn check_output(output: CommandOutput) -> Result<String> {
        if output.code != 0 {
            let stderr = sanitize_remote_output(&output.stderr);
            return Err(SshError::Command {
                code: output.code,
                stderr,
            }
            .into());
        }
        Ok(output.stdout)
    }

    pub async fn upload(&self, remote_path: &str, contents: &[u8]) -> Result<()> {
        use sha2::{Digest, Sha256};
        let digest = hex::encode(Sha256::digest(contents));
        crate::execution::remote("ssh-upload",crate::security::token_digest(&format!("{remote_path}:{digest}")),self.upload_remote(remote_path,contents),
            |_|serde_json::json!({"remote_path":remote_path,"content_digest":digest,"acknowledged":true})).await
    }

    async fn upload_remote(&self, remote_path: &str, contents: &[u8]) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(120), async {
            let channel = self.handle.channel_open_session().await?;
            channel.request_subsystem(true, "sftp").await?;
            let sftp = SftpSession::new(channel.into_stream()).await?;
            let mut file = sftp.create(remote_path).await?;
            file.write_all(contents).await?;
            file.close().await?;
            // `create` returns an open file handle, so close it before closing the
            // SFTP session to ensure all file data has reached the remote host.
            // The SFTP session's `write` convenience method only opens existing
            // files with `WRITE`, which fails for deployment staging files.
            sftp.close().await?;
            Ok(())
        })
        .await
        .map_err(|_| SshError::OperationTimedOut)?
    }

    pub async fn loopback_http_get(&self, port: u32, path: &str, secret: &str) -> Result<Vec<u8>> {
        self.loopback_http_request("GET", port, path, "127.0.0.1", Some(secret), None)
            .await
    }

    pub async fn loopback_http_get_target(
        &self,
        port: u32,
        path: &str,
        host: &str,
    ) -> Result<Vec<u8>> {
        self.loopback_http_request("GET", port, path, host, None, None)
            .await
    }

    pub async fn loopback_http_post(
        &self,
        port: u32,
        path: &str,
        secret: &str,
        body: &[u8],
    ) -> Result<Vec<u8>> {
        self.loopback_http_request("POST", port, path, "127.0.0.1", Some(secret), Some(body))
            .await
    }

    async fn loopback_http_request(
        &self,
        method: &str,
        port: u32,
        path: &str,
        host: &str,
        secret: Option<&str>,
        body: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        if method == "GET" {
            crate::execution::check_current().await?;
            return self
                .loopback_http_remote(method, port, path, host, secret, body)
                .await;
        }
        use sha2::{Digest, Sha256};
        let digest = hex::encode(Sha256::digest(body.unwrap_or_default()));
        crate::execution::remote(
            "ssh-http",
            crate::security::token_digest(&format!("{method}:{port}:{path}:{host}:{digest}")),
            self.loopback_http_remote(method, port, path, host, secret, body),
            |response| serde_json::json!({"response_digest":hex::encode(Sha256::digest(response))}),
        )
        .await
    }

    async fn loopback_http_remote(
        &self,
        method: &str,
        port: u32,
        path: &str,
        host: &str,
        secret: Option<&str>,
        body: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        let body = body.unwrap_or_default();
        let response = tokio::time::timeout(Duration::from_secs(crate::runtime_config::operation_timeout_seconds(5)), async {
            let mut channel = self
                .handle
                .channel_open_direct_tcpip("127.0.0.1", port, "127.0.0.1", 0)
                .await?;
            let authorization = secret
                .map(|secret| format!("Authorization: {secret}\r\n"))
                .unwrap_or_default();
            let request = format!(
                "{method} {path} HTTP/1.1\r\nHost: {host}\r\n{authorization}Accept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            channel.data(request.as_bytes()).await?;
            if !body.is_empty() {
                channel.data(body).await?;
            }
            let mut response = Vec::new();
            while let Some(message) = channel.wait().await {
                match message {
                    ChannelMsg::Data { data } => {
                        if data.len() > (16 * 1024 * 1024usize).saturating_sub(response.len()) {
                            return Err(SshError::OutputLimitExceeded.into());
                        }
                        response.extend_from_slice(&data);
                    },
                    ChannelMsg::Eof | ChannelMsg::Close => break,
                    _ => {}
                }
            }
            Ok::<Vec<u8>, anyhow::Error>(response)
        })
        .await
        .context("remote loopback HTTP request timed out")??;
        parse_http_response(&response)
    }
}

fn parse_http_response(response: &[u8]) -> Result<Vec<u8>> {
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("remote stats API returned an invalid HTTP response")?;
    let headers = String::from_utf8_lossy(&response[..separator]);
    let status_line = headers.lines().next().unwrap_or_default();
    if !status_line.contains(" 200 ") {
        bail!("remote stats API returned a non-success status")
    }
    let body = &response[separator + 4..];
    if headers.lines().any(|line| {
        line.to_ascii_lowercase()
            .starts_with("transfer-encoding: chunked")
    }) {
        decode_chunked(body)
    } else {
        Ok(body.to_vec())
    }
}

fn decode_chunked(mut body: &[u8]) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .context("invalid chunked HTTP response")?;
        let size_text = std::str::from_utf8(&body[..line_end])?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or_default(), 16)?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(output);
        }
        if body.len() < size + 2 {
            bail!("truncated chunked HTTP response")
        }
        output.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

fn sanitize_remote_output(value: &str) -> String {
    let filtered: String = value
        .chars()
        .filter(|character| !character.is_control() || *character == '\n' || *character == '\t')
        .take(1_000)
        .collect();
    if filtered.trim().is_empty() {
        "no diagnostic output".to_owned()
    } else {
        filtered
    }
}

pub async fn inspect_connected(session: &SshSession) -> Result<RemoteEnvironment> {
    let output = session
        .execute_read_only_checked(
            "set -eu; for tool in flock head wc sha256sum mkfifo grep; do command -v \"$tool\" >/dev/null; done; . /etc/os-release; printf '%s\\n%s\\n%s\\n' \"$ID\" \"$VERSION_ID\" \"$(uname -m)\"; command -v systemctl >/dev/null; systemctl --version | head -n1; if [ \"$(id -u)\" -eq 0 ]; then printf 'root\\n'; else sudo -n true && printf 'sudo\\n'; fi; df -Pk / | tail -n1",
        )
        .await?;
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() < 5 {
        bail!("remote host inspection returned incomplete system information")
    }
    let id = lines[0].trim().to_owned();
    let version = lines[1].trim().to_owned();
    let architecture = normalize_arch(lines[2].trim());
    let systemd = lines[3].trim().to_owned();
    let privilege = lines[4].trim().to_owned();
    if !matches!(id.as_str(), "debian" | "ubuntu") {
        bail!("unsupported Linux distribution: {id}")
    }
    if !supported_release(&id, &version) {
        bail!("unsupported {id} release {version}; use the tested Debian or Ubuntu versions")
    }
    if !matches!(architecture.as_str(), "amd64" | "arm64") {
        bail!("unsupported node architecture: {}", lines[2].trim())
    }
    if privilege != "root" && privilege != "sudo" {
        bail!("SSH user must be root or have non-interactive sudo")
    }
    Ok(RemoteEnvironment {
        distribution: id,
        version,
        architecture,
        privilege,
        fingerprint: session.fingerprint.clone(),
        systemd,
    })
}

#[derive(Clone, Debug)]
pub struct RemoteEnvironment {
    pub distribution: String,
    pub version: String,
    pub architecture: String,
    pub privilege: String,
    pub fingerprint: String,
    pub systemd: String,
}

pub fn normalize_arch(value: &str) -> String {
    match value {
        "x86_64" | "amd64" => "amd64".to_owned(),
        "aarch64" | "arm64" => "arm64".to_owned(),
        other => other.to_owned(),
    }
}

pub fn supported_release(id: &str, version: &str) -> bool {
    match id {
        "debian" => matches!(version, "12" | "13"),
        "ubuntu" => matches!(version, "22.04" | "24.04"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_arch, supported_release};

    #[test]
    fn normalizes_supported_linux_architectures() {
        assert_eq!(normalize_arch("x86_64"), "amd64");
        assert_eq!(normalize_arch("aarch64"), "arm64");
    }

    #[test]
    fn accepts_only_the_pinned_os_matrix() {
        assert!(supported_release("debian", "12"));
        assert!(supported_release("debian", "13"));
        assert!(supported_release("ubuntu", "22.04"));
        assert!(supported_release("ubuntu", "24.04"));
        assert!(!supported_release("ubuntu", "25.04"));
        assert!(!supported_release("centos", "9"));
    }
}

#[cfg(test)]
#[path = "ssh_execution_tests.rs"]
mod execution_tests;

#[cfg(test)]
#[path = "ssh_lifecycle_tests.rs"]
mod lifecycle_tests;
