//! Positive command acknowledgements; missing receipts never authorize recovery.
use crate::execution::remote::RemoteStep;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const RECEIPT_LIMIT: usize = 16 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Endpoint {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub fingerprint: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub protocol: u8,
    pub context: RemoteStep,
    pub state: String,
    pub exit_code: u32,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_sha256: String,
    pub stderr_sha256: String,
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn hash(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}
fn valid_head(value: &str) -> bool {
    value == "empty" || uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}
pub(crate) fn parse_head(text: &str) -> Result<String> {
    ensure!(text.len() <= 38, "remote predecessor is too large");
    let head = text.trim_end_matches('\n');
    ensure!(valid_head(head), "invalid remote predecessor");
    Ok(head.into())
}
pub(crate) fn head(resource: &str) -> Result<String> {
    ensure!(
        !resource.is_empty() && resource.len() <= 512,
        "invalid remote resource"
    );
    Ok(format!(
        "resource_hash={}\n{}",
        quote(&hash(resource.as_bytes())),
        include_str!("head.sh")
    ))
}
fn validate(step: &RemoteStep) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(&step.operation_id)
            .is_ok_and(|id| id.to_string() == step.operation_id)
            && uuid::Uuid::parse_str(&step.epoch).is_ok_and(|id| id.to_string() == step.epoch),
        "invalid remote execution identifier"
    );
    ensure!(
        step.generation > 0 && step.kind == "ssh-command",
        "invalid remote execution incarnation"
    );
    for value in [
        &step.resource,
        &step.owner,
        &step.work,
        &step.step,
        &step.request_digest,
    ] {
        ensure!(
            !value.is_empty() && value.len() <= 512 && !value.contains('\0'),
            "invalid remote execution binding"
        );
    }
    ensure!(
        step.predecessor.as_deref().is_some_and(valid_head),
        "remote predecessor is missing or invalid"
    );
    ensure!(
        serde_json::to_vec(step)?.len() <= 8 * 1024,
        "remote execution binding is too large"
    );
    Ok(())
}
fn prefix(step: &RemoteStep) -> Result<String> {
    validate(step)?;
    Ok(format!(
        "op_id={}\nresource_hash={}\nepoch={}\ngeneration={}\nbinding={}\nexpected_head={}\n",
        quote(&step.operation_id),
        quote(&hash(step.resource.as_bytes())),
        quote(&step.epoch),
        step.generation,
        quote(&hash(&serde_json::to_vec(&(&step.owner, &step.work))?)),
        quote(step.predecessor.as_deref().unwrap())
    ))
}
pub(crate) fn wrap(step: &RemoteStep, command: &str, cap: usize) -> Result<String> {
    ensure!(
        (1..=4 * 1024 * 1024).contains(&cap),
        "invalid command output limit"
    );
    ensure!(!command.contains('\0'), "invalid remote command");
    ensure!(
        step.request_digest == crate::security::token_digest(command),
        "remote command digest differs from its intent"
    );
    Ok(format!(
        "{}intent={}\ncommand={}\ncap={}\n{}",
        prefix(step)?,
        quote(&serde_json::to_string(step)?),
        quote(command),
        cap,
        include_str!("journal.sh")
    ))
}
pub(crate) fn inspect(step: &RemoteStep) -> Result<String> {
    Ok(format!("{}{}", prefix(step)?, include_str!("inspect.sh")))
}
pub(crate) fn parse(bytes: &[u8], step: &RemoteStep, cap: usize) -> Result<Receipt> {
    validate(step)?;
    ensure!(
        (1..=4 * 1024 * 1024).contains(&cap) && bytes.len() <= RECEIPT_LIMIT,
        "invalid remote receipt bounds"
    );
    let receipt: Receipt = serde_json::from_slice(bytes)?;
    ensure!(
        receipt.protocol == 2
            && receipt.state == "completed"
            && receipt.context == *step
            && receipt.exit_code <= 255,
        "remote completion receipt does not match its intent"
    );
    ensure!(
        receipt
            .stdout_bytes
            .checked_add(receipt.stderr_bytes)
            .is_some_and(|n| n <= cap as u64),
        "remote receipt exceeds output limit"
    );
    for digest in [&receipt.stdout_sha256, &receipt.stderr_sha256] {
        ensure!(
            digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid remote output digest"
        );
    }
    Ok(receipt)
}

#[cfg(test)]
mod tests;
