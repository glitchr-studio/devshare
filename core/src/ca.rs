//! The guest device's own certificate authority, and the certificates it
//! mints for the names of a session.
//!
//! Made once per device, its key kept on the device; trusted on this device
//! alone. It can only vouch for development names: its certificate carries
//! name constraints that permit the dev domains and the guest's own
//! `domain`, and exclude every IP address (see
//! [`devshare_protocol::authority`], which the helper checks before making
//! the system trust it). A browser on the guest then opens
//! `https://shop.test` without a warning, and without trusting anything of
//! the host's: the guest terminates TLS with a certificate of its own and
//! reaches the host's service with a client that accepts only the
//! certificate the host saw at share time.
//!
//! The key is a `0600` PKCS#8 file in the data folder on every platform for
//! now; the Keychain on macOS and the platforms' keystores on mobile are to
//! come.

use std::{
    collections::HashMap,
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use anyhow::{anyhow, bail, Context, Result};
use devshare_protocol::{
    authority::{self, Authority},
    names::{NamePolicy, DEV_DOMAINS},
    normalize_host,
};
use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, Issuer, KeyPair, KeyUsagePurpose,
    NameConstraints, PublicKeyData, SanType, SerialNumber, PKCS_ECDSA_P256_SHA256,
};
use rustls::{
    crypto::ring::default_provider,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    sign::CertifiedKey,
};
use time::OffsetDateTime;

use crate::invite::{data_folder, give_back};

const CERTIFICATE_FILE: &str = "ca.pem";
const KEY_FILE: &str = "ca.key";
/// Two years: renewed by hand, well before the helper's limit.
const VALIDITY: Duration = Duration::from_secs(730 * 24 * 3600);
/// A leaf lasts as long as its session, and never more than a day.
pub const LONGEST_LEAF: Duration = Duration::from_secs(24 * 3600);
/// Clocks of the device's programs may lag a little behind this one's.
const SKEW: Duration = Duration::from_secs(5 * 60);

/// This device's certificate authority, its key loaded.
pub struct DeviceCa {
    authority: Authority,
    pem: String,
    key_pem: String,
}

impl DeviceCa {
    /// A new authority for `domains` besides the dev domains, kept in memory
    /// only. See [`DeviceCa::create`] to keep one.
    pub fn generate(domains: &[String]) -> Result<Self> {
        let mut permitted: Vec<String> = DEV_DOMAINS
            .iter()
            .map(|domain| domain.to_string())
            .collect();
        for domain in domains {
            let domain = normalize_host(domain.trim_start_matches('.'));
            if domain.is_empty() || permitted.contains(&domain) {
                continue;
            }
            permitted.push(domain);
        }

        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let mut params = CertificateParams::default();
        let tag: [u8; 4] = rand::random();
        let tag: String = tag.iter().map(|byte| format!("{byte:02x}")).collect();
        let device = gethostname::gethostname().to_string_lossy().into_owned();
        let mut name = DistinguishedName::new();
        name.push(
            DnType::CommonName,
            format!("DevShare {} {tag}", crate::protocol::clean(&device, 40)),
        );
        name.push(DnType::OrganizationName, "DevShare, this device only");
        params.distinguished_name = name;
        params.serial_number = Some(serial());
        let now = SystemTime::now();
        params.not_before = (now - SKEW).into();
        params.not_after = (now + VALIDITY).into();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.name_constraints = Some(NameConstraints {
            permitted_subtrees: permitted
                .iter()
                .cloned()
                .map(GeneralSubtree::DnsName)
                .collect(),
            excluded_subtrees: vec![
                GeneralSubtree::IpAddress(CidrSubnet::V4([0; 4], [0; 4])),
                GeneralSubtree::IpAddress(CidrSubnet::V6([0; 16], [0; 16])),
            ],
        });
        let certificate = params.self_signed(&key)?;

        let policy = accepting(&permitted);
        Self::from_parts(certificate.pem(), key.serialize_pem(), &policy)
    }

    /// Makes this device's authority and keeps it, replacing any earlier
    /// one. The key file is readable by its owner only.
    pub fn create(domains: &[String]) -> Result<Self> {
        Self::create_in(&data_folder()?, domains)
    }

    fn create_in(folder: &Path, domains: &[String]) -> Result<Self> {
        let ca = Self::generate(domains)?;
        ca.save_in(folder)?;
        Ok(ca)
    }

    /// Keeps this authority as the device's, replacing any earlier one.
    pub fn save(&self) -> Result<()> {
        self.save_in(&data_folder()?)
    }

    fn save_in(&self, folder: &Path) -> Result<()> {
        let ca = self;
        fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
        give_back(folder);

        // Written aside, then renamed: a crash leaves the earlier pair whole.
        let key_path = folder.join(KEY_FILE);
        let staged = folder.join(format!("{KEY_FILE}.new"));
        fs::remove_file(&staged).ok();
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged)
            .with_context(|| format!("creating {}", staged.display()))?;
        file.write_all(ca.key_pem.as_bytes())?;
        file.sync_all()?;
        give_back(&staged);
        let certificate_path = folder.join(CERTIFICATE_FILE);
        let staged_certificate = folder.join(format!("{CERTIFICATE_FILE}.new"));
        fs::write(&staged_certificate, &ca.pem)?;
        give_back(&staged_certificate);
        fs::rename(&staged, &key_path)?;
        fs::rename(&staged_certificate, &certificate_path)?;
        Ok(())
    }

    /// This device's authority, if it made one. `domains` are the ones the
    /// guest accepts besides the dev domains; an authority permitting others
    /// is not used.
    pub fn load(domains: &[String]) -> Result<Option<Self>> {
        Self::load_from(&data_folder()?, domains)
    }

    fn load_from(folder: &Path, domains: &[String]) -> Result<Option<Self>> {
        let Ok(pem) = fs::read_to_string(folder.join(CERTIFICATE_FILE)) else {
            return Ok(None);
        };
        let key_path = folder.join(KEY_FILE);
        let key_pem = fs::read_to_string(&key_path)
            .with_context(|| format!("reading {}", key_path.display()))?;
        Self::from_parts(pem, key_pem, &accepting(domains)).map(Some)
    }

    /// Deletes this device's authority. Its trust must be removed first.
    pub fn delete() -> Result<()> {
        Self::delete_in(&data_folder()?)
    }

    fn delete_in(folder: &Path) -> Result<()> {
        for file in [KEY_FILE, CERTIFICATE_FILE] {
            match fs::remove_file(folder.join(file)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context(format!("deleting {file}")),
            }
        }
        Ok(())
    }

    /// Where the certificate is kept, for the guest to show or install.
    pub fn certificate_path() -> Result<PathBuf> {
        Ok(data_folder()?.join(CERTIFICATE_FILE))
    }

    fn from_parts(pem: String, key_pem: String, policy: &NamePolicy) -> Result<Self> {
        let authority = authority::check(&pem, policy).map_err(|reason| {
            anyhow!("this device's certificate authority is not usable: {reason}")
        })?;
        let key = KeyPair::from_pem(&key_pem).context("reading the authority's key")?;
        if key.subject_public_key_info() != authority.public_key {
            bail!("the authority's key does not match its certificate");
        }
        Ok(Self {
            authority,
            pem,
            key_pem,
        })
    }

    pub fn certificate_pem(&self) -> &str {
        &self.pem
    }

    pub fn certificate_der(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.authority.der.clone())
    }

    pub fn sha256(&self) -> &str {
        &self.authority.sha256
    }

    pub fn common_name(&self) -> &str {
        &self.authority.common_name
    }

    /// The domains it may vouch for.
    pub fn domains(&self) -> &[String] {
        &self.authority.domains
    }

    /// When it expires, in seconds since the Unix epoch.
    pub fn not_after(&self) -> i64 {
        self.authority.not_after
    }

    /// Mints the certificates of one session: for `names` only, valid until
    /// `until` and never longer than [`LONGEST_LEAF`].
    pub fn minter(&self, names: &[String], until: SystemTime) -> Result<Minter> {
        let key = KeyPair::from_pem(&self.key_pem)?;
        let issuer = Issuer::from_ca_cert_pem(&self.pem, key)?;
        let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let signing = default_provider()
            .key_provider
            .load_private_key(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                leaf_key.serialize_der(),
            )))
            .map_err(|error| anyhow!("loading the session's key: {error}"))?;
        let names = names
            .iter()
            .map(|name| normalize_host(name))
            .filter(|name| self.covers(name))
            .collect();
        Ok(Minter {
            issuer,
            leaf_key,
            signing,
            names,
            until: until.min(SystemTime::now() + LONGEST_LEAF),
            minted: Mutex::new(HashMap::new()),
        })
    }

    /// Whether the helper recorded that this computer trusts it.
    pub fn trusted(&self) -> bool {
        std::fs::read_to_string(devshare_protocol::helper::TRUSTED_CAS)
            .is_ok_and(|record| devshare_protocol::helper::records(&record, self.sha256()))
    }

    /// Whether its constraints let it vouch for `name`.
    pub fn covers(&self, name: &str) -> bool {
        let name = normalize_host(name);
        self.authority
            .domains
            .iter()
            .any(|domain| name == *domain || name.ends_with(&format!(".{domain}")))
    }
}

/// The certificates of one session, minted the first time a name is asked
/// for and kept until the session ends. One key signs for all of them.
pub struct Minter {
    issuer: Issuer<'static, KeyPair>,
    leaf_key: KeyPair,
    signing: Arc<dyn rustls::sign::SigningKey>,
    names: Vec<String>,
    until: SystemTime,
    minted: Mutex<HashMap<String, Arc<CertifiedKey>>>,
}

impl Minter {
    /// Whether it mints for `name`: a name of the session that the
    /// authority may vouch for.
    pub fn mints(&self, name: &str) -> bool {
        self.names.contains(&normalize_host(name))
    }

    /// The certificate for `name`, minted now if it was not yet.
    pub fn leaf(&self, name: &str) -> Result<Arc<CertifiedKey>> {
        let name = normalize_host(name);
        if !self.mints(&name) {
            bail!("{name} is not a name this session's certificates are for");
        }
        if let Some(leaf) = self.minted.lock().unwrap().get(&name) {
            return Ok(leaf.clone());
        }

        let mut params = CertificateParams::default();
        let mut subject = DistinguishedName::new();
        subject.push(DnType::CommonName, name.clone());
        params.distinguished_name = subject;
        params.subject_alt_names = vec![SanType::DnsName(name.clone().try_into()?)];
        params.serial_number = Some(serial());
        params.not_before = OffsetDateTime::from(SystemTime::now() - SKEW);
        params.not_after = OffsetDateTime::from(self.until);
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.use_authority_key_identifier_extension = true;
        let certificate = params.signed_by(&self.leaf_key, &self.issuer)?;

        let leaf = Arc::new(CertifiedKey::new(
            vec![certificate.der().clone()],
            self.signing.clone(),
        ));
        self.minted.lock().unwrap().insert(name, leaf.clone());
        Ok(leaf)
    }
}

/// A positive serial number of 16 random bytes.
fn serial() -> SerialNumber {
    let mut bytes: [u8; 16] = rand::random();
    bytes[0] &= 0x7f;
    bytes[0] |= 0x01;
    SerialNumber::from_slice(&bytes)
}

/// Names under the dev domains and `domains`, and no others.
fn accepting(domains: &[String]) -> NamePolicy {
    NamePolicy {
        domains: domains.to_vec(),
        trust_all: false,
    }
}

#[cfg(test)]
mod tests {
    use rustls::{
        client::{danger::ServerCertVerifier, WebPkiServerVerifier},
        pki_types::{ServerName, UnixTime},
        RootCertStore,
    };
    use x509_parser::prelude::{FromDer, X509Certificate};

    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// Verifies `certificate` for `name` the way a client trusting only
    /// `ca` would: rustls' own verifier, which enforces name constraints.
    fn verify(
        ca: &DeviceCa,
        certificate: &CertificateDer<'static>,
        name: &str,
    ) -> Result<(), String> {
        let mut roots = RootCertStore::empty();
        roots.add(ca.certificate_der()).unwrap();
        let verifier = WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::new(default_provider()),
        )
        .build()
        .unwrap();
        let name = ServerName::try_from(name.to_string()).map_err(|error| error.to_string())?;
        verifier
            .verify_server_cert(certificate, &[], &name, &[], UnixTime::now())
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// A certificate the authority's key signs whatever the minter would
    /// refuse: what a stolen key could make.
    fn forged(ca: &DeviceCa, san: SanType) -> CertificateDer<'static> {
        let issuer =
            Issuer::from_ca_cert_pem(&ca.pem, KeyPair::from_pem(&ca.key_pem).unwrap()).unwrap();
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let mut params = CertificateParams::default();
        params.subject_alt_names = vec![san];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.not_before = (SystemTime::now() - HOUR).into();
        params.not_after = (SystemTime::now() + HOUR).into();
        params.signed_by(&key, &issuer).unwrap().der().clone()
    }

    #[test]
    fn a_minted_certificate_is_for_its_name_as_a_server_until_the_session_ends() {
        let ca = DeviceCa::generate(&[]).unwrap();
        let until = SystemTime::now() + 2 * HOUR;
        let minter = ca
            .minter(&names(&["Shop.test", "api.shop.test"]), until)
            .unwrap();
        let leaf = minter.leaf("shop.test").unwrap();
        let der = &leaf.cert[0];

        let (_, parsed) = X509Certificate::from_der(der.as_ref()).unwrap();
        let sans: Vec<String> = parsed
            .subject_alternative_name()
            .unwrap()
            .unwrap()
            .value
            .general_names
            .iter()
            .map(|name| format!("{name}"))
            .collect();
        assert_eq!(sans, ["DNSName(shop.test)"]);
        assert!(
            parsed
                .extended_key_usage()
                .unwrap()
                .unwrap()
                .value
                .server_auth
        );
        assert!(!parsed.is_ca());
        let not_after = parsed.validity().not_after.timestamp();
        let expected = until
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!((not_after - expected).abs() <= 1, "{not_after} {expected}");

        verify(&ca, der, "shop.test").unwrap();
        assert!(verify(&ca, der, "api.shop.test").is_err());
        // Minted once, then kept.
        assert!(Arc::ptr_eq(&leaf, &minter.leaf("SHOP.test").unwrap()));
    }

    #[test]
    fn a_leaf_never_outlives_a_day() {
        let ca = DeviceCa::generate(&[]).unwrap();
        let minter = ca
            .minter(&names(&["shop.test"]), SystemTime::now() + 30 * 24 * HOUR)
            .unwrap();
        let leaf = minter.leaf("shop.test").unwrap();
        let (_, parsed) = X509Certificate::from_der(leaf.cert[0].as_ref()).unwrap();
        let validity = parsed.validity();
        let lasts = validity.not_after.timestamp() - validity.not_before.timestamp();
        assert!(lasts <= (LONGEST_LEAF + SKEW).as_secs() as i64, "{lasts}");
    }

    #[test]
    fn only_names_of_the_session_under_the_authority_are_minted() {
        let ca = DeviceCa::generate(&[]).unwrap();
        let minter = ca
            .minter(&names(&["shop.test", "shop.com"]), SystemTime::now() + HOUR)
            .unwrap();
        assert!(minter.mints("shop.test"));
        assert!(!minter.mints("shop.com"), "outside the constraints");
        assert!(!minter.mints("other.test"), "not a name of the session");
        assert!(minter.leaf("shop.com").is_err());
        assert!(minter.leaf("other.test").is_err());
    }

    #[test]
    fn even_its_own_key_cannot_vouch_for_a_real_site_or_an_address() {
        let ca = DeviceCa::generate(&[]).unwrap();
        let real = forged(&ca, SanType::DnsName("glitchr.dev".try_into().unwrap()));
        let error = verify(&ca, &real, "glitchr.dev").unwrap_err();
        assert!(
            error.contains("NameConstraint") || error.contains("constraint"),
            "{error}"
        );

        let address = forged(&ca, SanType::IpAddress("93.184.215.14".parse().unwrap()));
        assert!(verify(&ca, &address, "93.184.215.14").is_err());

        let fine = forged(&ca, SanType::DnsName("shop.test".try_into().unwrap()));
        verify(&ca, &fine, "shop.test").unwrap();
    }

    #[test]
    fn the_guest_s_own_domain_is_added_to_the_dev_domains() {
        let ca = DeviceCa::generate(&names(&[".Dev.Example.org"])).unwrap();
        assert!(ca.domains().contains(&"dev.example.org".to_string()));
        assert!(ca.domains().contains(&"test".to_string()));
        assert!(ca.covers("api.dev.example.org"));
        assert!(!ca.covers("example.org"));
        assert!(!ca.covers("notdev.example.org"));
    }

    #[test]
    fn it_is_kept_its_key_readable_by_its_owner_only_and_found_again() {
        use std::os::unix::fs::PermissionsExt;
        let folder = std::env::temp_dir().join(format!("devshare-ca-{}", rand::random::<u32>()));
        assert!(DeviceCa::load_from(&folder, &[]).unwrap().is_none());

        let made = DeviceCa::create_in(&folder, &[]).unwrap();
        let mode = fs::metadata(folder.join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let found = DeviceCa::load_from(&folder, &[]).unwrap().unwrap();
        assert_eq!(found.sha256(), made.sha256());

        // A certificate that does not go with the key is not used.
        let other = DeviceCa::generate(&[]).unwrap();
        fs::write(folder.join(CERTIFICATE_FILE), other.certificate_pem()).unwrap();
        let error = DeviceCa::load_from(&folder, &[]).err().unwrap().to_string();
        assert!(error.contains("does not match"), "{error}");

        DeviceCa::delete_in(&folder).unwrap();
        assert!(DeviceCa::load_from(&folder, &[]).unwrap().is_none());
        fs::remove_dir_all(&folder).ok();
    }
}
