//! In-process CA validation and signing. Secret-bearing values intentionally do
//! not implement Debug; callers persist payloads only through SecretBox.
use std::io::Cursor;

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, SerialNumber,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use x509_parser::{extensions::ParsedExtension, prelude::X509Certificate};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub client_validity_days: u16,
    pub renew_before_hours: u16,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            client_validity_days: 180,
            renew_before_hours: 720,
        }
    }
}

impl Policy {
    pub fn validate(self) -> Result<Self> {
        ensure!(
            (1..=365).contains(&self.client_validity_days),
            "client_validity_days must be 1–365"
        );
        ensure!(
            self.renew_before_hours >= 1
                && u32::from(self.renew_before_hours) < u32::from(self.client_validity_days) * 24,
            "renew_before_hours must be at least 1 and less than client validity"
        );
        Ok(self)
    }
}

pub struct Material {
    pub certificate: String,
    pub private_key: String,
}

impl Material {
    pub fn payload(&self) -> Value {
        json!({"certificate": self.certificate, "private_key": self.private_key})
    }
}

pub struct Authority {
    material: Material,
    pub expires_at: DateTime<Utc>,
    pub metadata: Value,
}

fn offset(value: DateTime<Utc>) -> Result<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(value.timestamp()).context("invalid certificate time")
}

fn certificate_pem(der: &[u8]) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
    for line in encoded.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    pem
}

fn serial() -> SerialNumber {
    let mut bytes = [0_u8; 20];
    rand::rng().fill_bytes(&mut bytes);
    // Positive, nonzero, RFC 5280's maximum 20 octets.
    bytes[0] = (bytes[0] & 0x7f) | 0x01;
    SerialNumber::from_slice(&bytes)
}

fn parameters(
    identity: &str,
    now: DateTime<Utc>,
    expiry: DateTime<Utc>,
) -> Result<CertificateParams> {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, identity);
    let mut params = CertificateParams::default();
    params.distinguished_name = dn;
    params.not_before = offset(now - Duration::minutes(5))?;
    params.not_after = offset(expiry)?;
    params.serial_number = Some(serial());
    Ok(params)
}

impl Authority {
    pub fn generate(now: DateTime<Utc>) -> Result<Self> {
        let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
        let mut params = parameters(
            &format!("hx-ca-{}", uuid::Uuid::new_v4()),
            now,
            now + Duration::days(3650),
        )?;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let material = Material {
            certificate: params.self_signed(&key)?.pem(),
            private_key: key.serialize_pem(),
        };
        Self::import(material, now)
    }

    /// The first certificate is the selected issuer. Remaining certificates
    /// prove its chain, but are never exported as the node's trust anchors.
    pub fn import(mut material: Material, now: DateTime<Utc>) -> Result<Self> {
        ensure!(
            material.certificate.len() + material.private_key.len() <= 2 * 1024 * 1024,
            "CA payload exceeds 2 MiB"
        );
        let certs = rustls_pemfile::certs(&mut Cursor::new(material.certificate.as_bytes()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("invalid CA certificate PEM")?;
        ensure!(
            !certs.is_empty() && certs.len() <= 16,
            "CA chain must contain 1–16 certificates"
        );
        // PEM readers skip unrelated blocks and text. Never echo the original
        // input into public metadata: it could contain an embedded private key.
        let canonical: String = certs
            .iter()
            .map(|cert| certificate_pem(cert.as_ref()))
            .collect();
        ensure!(
            material.certificate.split_whitespace().collect::<String>()
                == canonical.split_whitespace().collect::<String>(),
            "CA input must contain only certificate PEM blocks"
        );
        material.certificate = canonical;
        let unique: std::collections::BTreeSet<_> = certs.iter().map(|der| der.as_ref()).collect();
        ensure!(
            unique.len() == certs.len(),
            "duplicate certificate in CA chain"
        );
        let parsed: Vec<_> = certs
            .iter()
            .map(|der| {
                let (remaining, parsed) = x509_parser::parse_x509_certificate(der.as_ref())
                    .map_err(|_| anyhow::anyhow!("invalid CA certificate"))?;
                ensure!(remaining.is_empty(), "trailing certificate data");
                Ok(parsed)
            })
            .collect::<Result<_>>()?;
        for (index, cert) in parsed.iter().enumerate() {
            validate_certificate(cert, now)?;
            let constraints = cert
                .basic_constraints()?
                .context("CA basic constraint required")?;
            if let Some(limit) = constraints.value.path_len_constraint {
                let below = parsed[..index]
                    .iter()
                    .filter(|c| c.subject() != c.issuer())
                    .count();
                ensure!(below <= limit as usize, "CA path length exceeded");
            }
            if let Some(parent) = parsed.get(index + 1) {
                ensure!(
                    cert.issuer() == parent.subject(),
                    "CA chain issuer mismatch"
                );
                cert.verify_signature(Some(parent.public_key()))
                    .map_err(|_| anyhow::anyhow!("invalid or unsupported CA chain signature"))?;
            } else {
                ensure!(
                    cert.issuer() == cert.subject(),
                    "CA chain must end in a self-signed root"
                );
                cert.verify_signature(None)
                    .map_err(|_| anyhow::anyhow!("invalid or unsupported CA root signature"))?;
            }
        }
        let key = KeyPair::from_pem(&material.private_key)
            .map_err(|_| anyhow::anyhow!("invalid or unsupported CA private key"))?;
        use rcgen::PublicKeyData;
        ensure!(
            key.subject_public_key_info() == parsed[0].public_key().raw,
            "CA certificate and private key do not match"
        );
        // Ensure rcgen can preserve this issuer's DN and key identifier before
        // admitting it to the vault, rather than failing during authorization.
        Issuer::from_ca_cert_der(&certs[0], key)
            .map_err(|_| anyhow::anyhow!("unsupported issuing CA"))?;
        let expires_at = DateTime::from_timestamp(parsed[0].validity().not_after.timestamp(), 0)
            .context("invalid CA expiry")?;
        let metadata = super::certificate_metadata(&material.certificate)?;
        Ok(Self {
            material,
            expires_at,
            metadata,
        })
    }

    pub fn from_payload(payload: &Value, now: DateTime<Utc>) -> Result<Self> {
        let object = payload
            .as_object()
            .context("CA payload must be an object")?;
        ensure!(
            object
                .keys()
                .all(|k| matches!(k.as_str(), "certificate" | "private_key")),
            "unexpected CA payload field"
        );
        Self::import(
            Material {
                certificate: super::text(payload, "certificate")?.into(),
                private_key: super::text(payload, "private_key")?.into(),
            },
            now,
        )
    }

    pub fn payload(&self) -> Value {
        self.material.payload()
    }

    pub fn trust_anchor(&self) -> Result<String> {
        let der = rustls_pemfile::certs(&mut Cursor::new(self.material.certificate.as_bytes()))
            .next()
            .context("CA certificate required")??;
        Ok(certificate_pem(der.as_ref()))
    }

    pub fn validate_new_binding(&self, now: DateTime<Utc>) -> Result<()> {
        ensure!(
            self.expires_at > now + Duration::days(30),
            "CA must have more than 30 days remaining"
        );
        Ok(())
    }

    pub fn sign_user(
        &self,
        identity_id: &uuid::Uuid,
        policy: Policy,
        now: DateTime<Utc>,
    ) -> Result<Issued> {
        policy.validate()?;
        self.sign(
            &format!("hx-user-{identity_id}"),
            Duration::days(policy.client_validity_days.into()),
            Duration::hours(policy.renew_before_hours.into()),
            now,
        )
    }

    pub fn sign_probe(&self, node_id: &uuid::Uuid, now: DateTime<Utc>) -> Result<Issued> {
        self.sign(
            &format!("hx-probe-{node_id}"),
            Duration::days(7),
            Duration::hours(48),
            now,
        )
    }

    fn sign(
        &self,
        identity: &str,
        validity: Duration,
        renew_before: Duration,
        now: DateTime<Utc>,
    ) -> Result<Issued> {
        let now = DateTime::from_timestamp(now.timestamp(), 0).context("invalid signing time")?;
        let expiry = (now + validity).min(self.expires_at - Duration::minutes(5));
        ensure!(
            expiry > now,
            "CA update required: cannot issue a valid certificate"
        );
        let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
        let mut params = parameters(identity, now, expiry)?;
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        params.use_authority_key_identifier_extension = true;
        let issuer_key = KeyPair::from_pem(&self.material.private_key)
            .map_err(|_| anyhow::anyhow!("invalid CA signing key"))?;
        let der = rustls_pemfile::certs(&mut Cursor::new(self.material.certificate.as_bytes()))
            .next()
            .context("CA certificate required")??;
        let issuer = Issuer::from_ca_cert_der(&der, issuer_key)?;
        Ok(Issued {
            material: Material {
                certificate: params.signed_by(&key, &issuer)?.pem(),
                private_key: key.serialize_pem(),
            },
            not_before: now - Duration::minutes(5),
            expires_at: expiry,
            next_action_at: expiry - renew_before,
        })
    }
}

pub struct Issued {
    pub material: Material,
    pub not_before: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub next_action_at: DateTime<Utc>,
}

fn validate_certificate(cert: &X509Certificate<'_>, now: DateTime<Utc>) -> Result<()> {
    ensure!(
        cert.validity().not_before.timestamp() <= now.timestamp()
            && cert.validity().not_after.timestamp() > now.timestamp(),
        "CA certificate is not currently valid"
    );
    ensure!(
        cert.signature_algorithm == cert.tbs_certificate.signature,
        "CA signature algorithms disagree"
    );
    ensure!(
        cert.basic_constraints()?.is_some_and(|c| c.value.ca),
        "CA basic constraint required"
    );
    ensure!(
        cert.key_usage()?.is_none_or(|u| u.value.key_cert_sign()),
        "CA key usage does not permit signing"
    );
    ensure!(
        cert.extended_key_usage()?
            .is_none_or(|u| u.value.any || u.value.client_auth),
        "CA purpose excludes client authentication"
    );
    cert.extensions_map()
        .map_err(|_| anyhow::anyhow!("duplicate CA extension"))?;
    for ext in cert.extensions() {
        match ext.parsed_extension() {
            ParsedExtension::NameConstraints(_)
            | ParsedExtension::PolicyMappings(_)
            | ParsedExtension::PolicyConstraints(_)
            | ParsedExtension::InhibitAnyPolicy(_) => {
                bail!("unsupported CA name or policy constraints")
            }
            ParsedExtension::ParseError { .. } | ParsedExtension::Unparsed => {
                bail!("invalid CA extension")
            }
            ParsedExtension::BasicConstraints(_)
            | ParsedExtension::KeyUsage(_)
            | ParsedExtension::ExtendedKeyUsage(_)
            | ParsedExtension::AuthorityKeyIdentifier(_)
            | ParsedExtension::SubjectKeyIdentifier(_) => (),
            _ if ext.critical => bail!("unsupported critical CA extension"),
            _ => (),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
