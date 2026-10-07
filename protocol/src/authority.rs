//! What a device's own certificate authority must be for the helper to make
//! the system trust it, and what the guest reads back from it.
//!
//! The rules are the reason trusting it is acceptable at all: a certificate
//! authority that only the guest's device trusts, and that can only vouch
//! for development names. Its certificate must carry name constraints
//! (RFC 5280) whose permitted subtrees are DNS names inside the domains the
//! helper accepts for a session's names, and whose excluded subtrees cover
//! every IPv4 and IPv6 address: without that exclusion, a constraint on DNS
//! names leaves certificates for IP addresses unconstrained.

use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use x509_parser::{
    extensions::{GeneralName, GeneralSubtree},
    pem::parse_x509_pem,
    prelude::{FromDer, X509Certificate},
};

use crate::names::NamePolicy;

/// The longest validity accepted, a day of margin over the two years a
/// device's authority is made for.
pub const LONGEST_VALIDITY: i64 = 731 * 24 * 3600;

/// Upper bound of a certificate in PEM form; a device's is about 1 KiB.
pub const MAX_PEM: usize = 16 * 1024;

/// A certificate authority that passed [`check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authority {
    /// SHA-256 of the certificate, in hexadecimal.
    pub sha256: String,
    pub common_name: String,
    /// The permitted DNS subtrees, lowercased, without a leading dot.
    pub domains: Vec<String>,
    /// Seconds since the Unix epoch.
    pub not_after: i64,
    pub der: Vec<u8>,
    /// Its `subjectPublicKeyInfo`, in DER.
    pub public_key: Vec<u8>,
}

/// Reads a certificate in PEM form and checks that it is a device's own
/// authority, constrained to names `policy` accepts. `policy` must not trust
/// every name: such a policy accepts no authority.
pub fn check(pem: &str, policy: &NamePolicy) -> Result<Authority, String> {
    if pem.len() > MAX_PEM {
        return Err("the certificate is too large".into());
    }
    if pem.contains("PRIVATE KEY") {
        return Err("a private key was sent: only the certificate is ever needed".into());
    }
    let (rest, block) = parse_x509_pem(pem.as_bytes()).map_err(|_| "not a PEM certificate")?;
    if block.label != "CERTIFICATE" {
        return Err(format!(
            "a PEM block of {} is not a certificate",
            block.label
        ));
    }
    if parse_x509_pem(rest).is_ok() {
        return Err("only one certificate is accepted".into());
    }
    check_der(&block.contents, policy, now())
}

/// [`check`], on the certificate's DER encoding, at `now` (Unix seconds).
pub fn check_der(der: &[u8], policy: &NamePolicy, now: i64) -> Result<Authority, String> {
    if policy.trust_all {
        return Err("no authority is accepted when every name is".into());
    }
    let (rest, certificate) =
        X509Certificate::from_der(der).map_err(|error| format!("not a certificate: {error}"))?;
    if !rest.is_empty() {
        return Err("data follows the certificate".into());
    }

    let is_ca = certificate
        .basic_constraints()
        .ok()
        .flatten()
        .is_some_and(|extension| extension.value.ca);
    if !is_ca {
        return Err("not a certificate authority".into());
    }
    let signs_certificates = certificate
        .key_usage()
        .ok()
        .flatten()
        .is_some_and(|extension| extension.value.key_cert_sign());
    if !signs_certificates {
        return Err("its key usage does not include signing certificates".into());
    }
    certificate
        .verify_signature(None)
        .map_err(|_| "not self-signed: only a root of this device's own is accepted")?;

    let validity = certificate.validity();
    let (not_before, not_after) = (
        validity.not_before.timestamp(),
        validity.not_after.timestamp(),
    );
    if not_after - not_before > LONGEST_VALIDITY {
        return Err("valid for more than two years".into());
    }
    if not_after <= now || not_before > now + 24 * 3600 {
        return Err("not valid now".into());
    }

    let constraints = certificate
        .name_constraints()
        .map_err(|_| "its name constraints cannot be read")?
        .ok_or("no name constraints: it could vouch for any site")?
        .value;
    let permitted = constraints
        .permitted_subtrees
        .as_deref()
        .filter(|subtrees| !subtrees.is_empty())
        .ok_or("no permitted names: it could vouch for any site")?;
    let mut domains = Vec::new();
    for subtree in permitted {
        let GeneralName::DNSName(name) = subtree.base else {
            return Err("it permits names other than DNS names".into());
        };
        let name = name.trim_start_matches('.').to_ascii_lowercase();
        if !policy.accepts(&name) {
            return Err(format!(
                "it permits names under {}, outside the domains this device accepts",
                crate::clean(&name, 80)
            ));
        }
        domains.push(name);
    }
    let excluded = constraints.excluded_subtrees.as_deref().unwrap_or_default();
    if !excludes_everything(excluded, 4) || !excludes_everything(excluded, 16) {
        return Err("it does not exclude every IP address".into());
    }

    let common_name = certificate
        .subject()
        .iter_common_name()
        .next()
        .and_then(|name| name.as_str().ok())
        .unwrap_or_default()
        .to_string();
    Ok(Authority {
        sha256: sha256(der),
        common_name,
        domains,
        not_after,
        der: der.to_vec(),
        public_key: certificate.tbs_certificate.subject_pki.raw.to_vec(),
    })
}

/// Whether the subtrees exclude all addresses of `length` bytes: an address
/// and a mask of zeros.
fn excludes_everything(subtrees: &[GeneralSubtree], length: usize) -> bool {
    subtrees.iter().any(|subtree| {
        matches!(subtree.base, GeneralName::IPAddress(bytes)
            if bytes.len() == 2 * length && bytes.iter().all(|byte| *byte == 0))
    })
}

pub fn sha256(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use rcgen::{
        BasicConstraints, CertificateParams, CidrSubnet, GeneralSubtree, IsCa, Issuer, KeyPair,
        KeyUsagePurpose, NameConstraints, PKCS_ECDSA_P256_SHA256,
    };

    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 3600);
    use std::time::Duration;

    fn everything() -> Vec<GeneralSubtree> {
        vec![
            GeneralSubtree::IpAddress(CidrSubnet::V4([0; 4], [0; 4])),
            GeneralSubtree::IpAddress(CidrSubnet::V6([0; 16], [0; 16])),
        ]
    }

    /// What a device makes, before each test spoils one part of it.
    fn device_params() -> CertificateParams {
        let mut params = CertificateParams::default();
        let now = SystemTime::now();
        params.not_before = (now - DAY).into();
        params.not_after = (now + 700 * DAY).into();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.name_constraints = Some(NameConstraints {
            permitted_subtrees: vec![
                GeneralSubtree::DnsName("test".into()),
                GeneralSubtree::DnsName("localhost".into()),
            ],
            excluded_subtrees: everything(),
        });
        params
    }

    fn pem(params: CertificateParams) -> String {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        params.self_signed(&key).unwrap().pem()
    }

    fn checked(params: CertificateParams) -> Result<Authority, String> {
        check(&pem(params), &NamePolicy::with(None))
    }

    #[test]
    fn a_device_authority_constrained_to_dev_names_is_accepted() {
        let authority = checked(device_params()).unwrap();
        assert_eq!(authority.domains, ["test", "localhost"]);
        assert_eq!(authority.sha256.len(), 64);
        assert!(!authority.public_key.is_empty());
    }

    #[test]
    fn an_authority_without_name_constraints_is_refused() {
        let mut params = device_params();
        params.name_constraints = None;
        assert!(checked(params).unwrap_err().contains("no name constraints"));
    }

    #[test]
    fn an_authority_for_a_real_domain_is_refused() {
        for domain in ["com", "glitchr.dev", ""] {
            let mut params = device_params();
            params.name_constraints = Some(NameConstraints {
                permitted_subtrees: vec![
                    GeneralSubtree::DnsName("test".into()),
                    GeneralSubtree::DnsName(domain.into()),
                ],
                excluded_subtrees: everything(),
            });
            let error = checked(params).unwrap_err();
            assert!(error.contains("outside the domains"), "{domain}: {error}");
        }
        // Unless the device was told to accept it.
        let mut params = device_params();
        params.name_constraints = Some(NameConstraints {
            permitted_subtrees: vec![GeneralSubtree::DnsName("dev.glitchr.dev".into())],
            excluded_subtrees: everything(),
        });
        let policy = NamePolicy::with(Some("dev.glitchr.dev".into()));
        assert!(check(&pem(params), &policy).is_ok());
    }

    #[test]
    fn an_authority_that_leaves_ip_addresses_open_is_refused() {
        for excluded in [
            vec![],
            everything()[..1].to_vec(),
            everything()[1..].to_vec(),
        ] {
            let mut params = device_params();
            params.name_constraints = Some(NameConstraints {
                permitted_subtrees: vec![GeneralSubtree::DnsName("test".into())],
                excluded_subtrees: excluded,
            });
            assert!(checked(params).unwrap_err().contains("every IP address"));
        }
        let mut params = device_params();
        params.name_constraints = Some(NameConstraints {
            permitted_subtrees: vec![
                GeneralSubtree::DnsName("test".into()),
                GeneralSubtree::IpAddress(CidrSubnet::V4([10, 0, 0, 0], [255, 0, 0, 0])),
            ],
            excluded_subtrees: everything(),
        });
        assert!(checked(params)
            .unwrap_err()
            .contains("other than DNS names"));
    }

    #[test]
    fn what_is_not_a_root_of_its_own_is_refused() {
        let mut params = device_params();
        params.is_ca = IsCa::NoCa;
        assert!(checked(params)
            .unwrap_err()
            .contains("not a certificate authority"));

        let mut params = device_params();
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        assert!(checked(params).unwrap_err().contains("key usage"));

        // Issued by another authority: an intermediate, not this device's root.
        let parent_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let parent = Issuer::new(device_params(), parent_key);
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let mut child = device_params();
        child
            .distinguished_name
            .push(rcgen::DnType::CommonName, "child");
        let pem = child.signed_by(&key, &parent).unwrap().pem();
        let error = check(&pem, &NamePolicy::with(None)).unwrap_err();
        assert!(error.contains("not self-signed"), "{error}");
    }

    #[test]
    fn an_authority_valid_too_long_or_not_now_is_refused() {
        let mut params = device_params();
        params.not_after = (SystemTime::now() + 800 * DAY).into();
        assert!(checked(params).unwrap_err().contains("more than two years"));

        let mut params = device_params();
        params.not_before = (SystemTime::now() - 10 * DAY).into();
        params.not_after = (SystemTime::now() - DAY).into();
        assert!(checked(params).unwrap_err().contains("not valid now"));
    }

    #[test]
    fn only_one_certificate_and_never_a_key_is_accepted() {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let certificate = device_params().self_signed(&key).unwrap().pem();
        let policy = NamePolicy::with(None);

        let with_key = format!("{certificate}{}", key.serialize_pem());
        assert!(check(&with_key, &policy)
            .unwrap_err()
            .contains("private key"));
        let twice = format!("{certificate}{certificate}");
        assert!(check(&twice, &policy).unwrap_err().contains("only one"));
        assert!(check("hello", &policy).is_err());
        let trusting = NamePolicy {
            domains: vec![],
            trust_all: true,
        };
        assert!(check(&certificate, &trusting).is_err());
    }
}
