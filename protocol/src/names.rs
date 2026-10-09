//! Which hostnames a guest lets a session give it.
//!
//! The guest resolves the session's names to the tunnel for as long as it is
//! joined. A host that named `accounts.google.com` would therefore receive
//! the guest's traffic for it: the names a session may use are the ones that
//! never exist on the internet, unless the guest says otherwise.
//!
//! Here rather than in the guest's code because the privileged helper, which
//! installs those names for a guest without administrator rights, applies the
//! same rules on its own.

use crate::Manifest;

/// Domains reserved for local and test use, which no public name ends with.
pub const DEV_DOMAINS: &[&str] = &[
    "test",
    "localhost",
    "example",
    "invalid",
    "internal",
    "home.arpa",
    // Reserved for names a local network gives itself (RFC 6762): many
    // development setups use it, and no public site can.
    "local",
];

/// Letters, digits and hyphens in labels of 1 to 63, up to 253 in all.
pub fn is_hostname(name: &str) -> bool {
    let label = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    !name.is_empty() && name.len() <= 253 && name.split('.').all(label)
}

#[derive(Debug, Clone, Default)]
pub struct NamePolicy {
    /// Domains accepted besides [`DEV_DOMAINS`]: the guest's own `domain`
    /// setting, typically.
    pub domains: Vec<String>,
    /// Accept any name. For a guest who knows what the session names and why.
    pub trust_all: bool,
}

impl NamePolicy {
    pub fn with(domain: Option<String>) -> Self {
        Self {
            domains: domain.into_iter().collect(),
            trust_all: false,
        }
    }

    pub fn accepts(&self, name: &str) -> bool {
        if !is_hostname(name) {
            return false;
        }
        if self.trust_all {
            return true;
        }
        DEV_DOMAINS
            .iter()
            .copied()
            .chain(self.domains.iter().map(String::as_str))
            .filter(|domain| !domain.is_empty())
            .any(|domain| name == domain || name.ends_with(&format!(".{domain}")))
    }

    /// The names of the session this policy does not accept.
    pub fn refused(&self, manifest: &Manifest) -> Vec<String> {
        manifest
            .hostnames()
            .into_iter()
            .filter(|name| !self.accepts(name))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{Environment, SessionInfo};

    fn manifest(names: &[&str]) -> Manifest {
        Manifest {
            protocol: 1,
            manifest_version: 1,
            session: SessionInfo {
                id: "s".into(),
                lifetime: 300,
                expires_in: 300,
                max_guests: 3,
            },
            environments: BTreeMap::from([(
                "shop".to_string(),
                Environment {
                    entrypoint: None,
                    dns: names.iter().map(|name| name.to_string()).collect(),
                    services: Vec::new(),
                    launches: Vec::new(),
                },
            )]),
        }
    }

    #[test]
    fn only_names_that_cannot_exist_on_the_internet_pass_by_default() {
        let session = manifest(&[
            "shop.test",
            "docs.localhost",
            "accounts.google.com",
            "shop.lan",
            "test.example.com",
        ]);
        let refused = NamePolicy::default().refused(&session);
        assert_eq!(
            refused,
            ["accounts.google.com", "shop.lan", "test.example.com"]
        );

        // The guest's own domain counts; a domain is not a suffix of a label.
        let lan = NamePolicy::with(Some("lan".into()));
        assert_eq!(
            lan.refused(&session),
            ["accounts.google.com", "test.example.com"]
        );
        assert_eq!(
            NamePolicy::with(Some("com".into())).refused(&manifest(&["evil.xcom"])),
            ["evil.xcom"]
        );
        assert!(NamePolicy {
            trust_all: true,
            ..Default::default()
        }
        .refused(&session)
        .is_empty());
    }

    #[test]
    fn a_hostname_is_made_of_labels_and_nothing_else() {
        for bad in [
            "../../etc/x",
            "shop test",
            "shop.test;rm",
            "-shop.test",
            "",
            "a..b",
            "Shop.test",
            &"a".repeat(64),
        ] {
            assert!(!is_hostname(bad), "{bad:?}");
        }
        assert!(is_hostname("api-v2.shop.test"));
    }
}
