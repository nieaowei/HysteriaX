use super::*;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub id: String,
    pub original: RemoteStep,
    pub receipt_sha256: String,
    pub target_epoch: String,
    pub target_generation: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Proof {
    pub protocol: u8,
    pub request: Request,
    pub completion: Receipt,
    pub state: String,
}
fn validate_request(r: &Request) -> Result<()> {
    validate(&r.original)?;
    ensure!(
        uuid::Uuid::parse_str(&r.id).is_ok_and(|id| id.to_string() == r.id)
            && uuid::Uuid::parse_str(&r.target_epoch)
                .is_ok_and(|id| id.to_string() == r.target_epoch),
        "invalid verification identifier"
    );
    ensure!(
        r.target_generation > r.original.generation
            && r.receipt_sha256.len() == 64
            && r.receipt_sha256.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid verification target"
    );
    Ok(())
}
pub(crate) fn digest(receipt: &Receipt) -> Result<String> {
    Ok(hash(
        format!("{}\n", serde_json::to_string(receipt)?).as_bytes(),
    ))
}
pub(crate) fn render(r: &Request) -> Result<String> {
    validate_request(r)?;
    Ok(format!(
        "{}check_id={}\nrequest={}\nrequest_hash={}\nreceipt_hash={}\ntarget_epoch={}\ntarget_generation={}\ntarget_binding={}\n{}",
        prefix(&r.original)?,
        quote(&r.id),
        quote(&serde_json::to_string(r)?),
        quote(&hash(&serde_json::to_vec(r)?)),
        quote(&r.receipt_sha256),
        quote(&r.target_epoch),
        r.target_generation,
        quote(&hash(&serde_json::to_vec(&("recheck", &r.id))?)),
        include_str!("recheck/recheck.sh")
    ))
}
pub(crate) fn parse(bytes: &[u8], r: &Request) -> Result<Proof> {
    validate_request(r)?;
    ensure!(
        bytes.len() <= RECEIPT_LIMIT,
        "verification proof is too large"
    );
    let proof: Proof = serde_json::from_slice(bytes)?;
    ensure!(
        proof.protocol == 2 && proof.state == "fenced" && proof.request == *r,
        "verification proof does not match its intent"
    );
    super::parse(
        &serde_json::to_vec(&proof.completion)?,
        &r.original,
        4 * 1024 * 1024,
    )?;
    ensure!(
        digest(&proof.completion)? == r.receipt_sha256,
        "original completion proof changed"
    );
    Ok(proof)
}
