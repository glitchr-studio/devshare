//! Which hostnames a guest lets a session give it.
//!
//! The guest resolves the session's names to the tunnel for as long as it is
//! joined. A host that named `accounts.google.com` would therefore receive
//! the guest's traffic for it: the names a session may use are the ones that
//! never exist on the internet, unless the guest says otherwise.

use devshare_protocol::Manifest;

/// Domains reserved for local and test use, which no public name ends with.
pub const DEV_DOMAINS: &[&str] = &[
    "test",
    "localhost",
    "example",
    "invalid",
    "internal",
    "home.arpa",
];

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

    fn accepts(&self, name: &str) -> bool {
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
    use super::*;
    use crate::guest::addresses::tests::manifest;

    #[test]
    fn only_names_that_cannot_exist_on_the_internet_pass_by_default() {
        let session = manifest(&[
            ("shop.test", 443),
            ("docs.localhost", 80),
            ("accounts.google.com", 443),
            ("shop.lan", 80),
            ("test.example.com", 80),
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
            NamePolicy::with(Some("com".into())).refused(&manifest(&[("evil.xcom", 80)])),
            ["evil.xcom"]
        );
        assert!(NamePolicy {
            trust_all: true,
            ..Default::default()
        }
        .refused(&session)
        .is_empty());
    }
}
