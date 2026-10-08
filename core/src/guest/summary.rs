//! What a guest is shown of a session it joined: its environments, and for
//! each service the address a browser opens. Everything in it comes from
//! the host: names are cleaned, and addresses are rebuilt from names and
//! ports rather than taken as the host wrote them.

use std::collections::HashSet;

use devshare_protocol::{clean, Manifest};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub environments: Vec<SummaryEnvironment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SummaryEnvironment {
    pub name: String,
    /// Where to start, as the host declared it, when it is one of the
    /// session's own addresses.
    pub entrypoint: Option<String>,
    pub services: Vec<SummaryService>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SummaryService {
    /// `shop.test:443`.
    pub address: String,
    pub host: String,
    pub port: u16,
    /// What a browser opens: `https://shop.test`.
    pub url: String,
    /// SHA-256 of the certificate the host saw, when the service speaks TLS.
    pub sha256: Option<String>,
    /// Certified by this device's own authority: opens without a warning.
    pub certified: bool,
}

impl Summary {
    /// `certified` lists the services, by lowercased name and port, that
    /// this device terminates with its own certificates.
    pub fn of(manifest: &Manifest, certified: &HashSet<(String, u16)>) -> Self {
        let environments = manifest
            .environments
            .iter()
            .map(|(name, environment)| {
                let services: Vec<SummaryService> = environment
                    .services
                    .iter()
                    .map(|service| {
                        let host = service.host.to_ascii_lowercase();
                        let sha256 = service.tls.as_ref().map(|tls| clean(&tls.sha256, 64));
                        SummaryService {
                            address: format!("{host}:{}", service.port),
                            url: url(&host, service.port, sha256.is_some()),
                            certified: certified.contains(&(host.clone(), service.port)),
                            host,
                            port: service.port,
                            sha256,
                        }
                    })
                    .collect();
                let entrypoint = environment.entrypoint.as_deref().and_then(|entrypoint| {
                    let parsed = url::Url::parse(entrypoint).ok()?;
                    if !matches!(parsed.scheme(), "http" | "https") {
                        return None;
                    }
                    let host = parsed.host_str()?.to_ascii_lowercase();
                    let port = parsed.port_or_known_default()?;
                    let start = url(&host, port, parsed.scheme() == "https");
                    services
                        .iter()
                        .any(|service| service.url == start)
                        .then(|| {
                            // Percent-encoded by the parser: nothing but a path.
                            format!("{start}{}", parsed.path().trim_end_matches('/'))
                        })
                });
                SummaryEnvironment {
                    name: clean(name, 64),
                    entrypoint,
                    services,
                }
            })
            .collect();
        Self { environments }
    }

    /// Every address of the session: the only ones a guest's window may
    /// open.
    pub fn urls(&self) -> HashSet<String> {
        self.environments
            .iter()
            .flat_map(|environment| {
                environment
                    .services
                    .iter()
                    .map(|service| service.url.clone())
                    .chain(environment.entrypoint.clone())
            })
            .collect()
    }
}

/// `https://shop.test`, `http://shop.test:5173`.
pub fn url(host: &str, port: u16, tls: bool) -> String {
    match (tls, port) {
        (true, 443) => format!("https://{host}"),
        (false, 80) => format!("http://{host}"),
        (true, port) => format!("https://{host}:{port}"),
        (false, port) => format!("http://{host}:{port}"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use devshare_protocol::{Environment, Service, SessionInfo, Tls, Transport};

    use super::*;

    fn manifest(entrypoint: &str) -> Manifest {
        let service = |host: &str, port, tls: bool| Service {
            host: host.into(),
            port,
            protocol: Transport::Tcp,
            tls: tls.then(|| Tls {
                sha256: "ab".repeat(32),
            }),
        };
        Manifest {
            protocol: 2,
            manifest_version: 1,
            session: SessionInfo {
                id: "s".into(),
                lifetime: 300,
                expires_in: 120,
                max_guests: 3,
            },
            environments: BTreeMap::from([(
                "shop".to_string(),
                Environment {
                    entrypoint: Some(entrypoint.into()),
                    dns: vec![],
                    services: vec![
                        service("Shop.test", 443, true),
                        service("shop.test", 5173, false),
                    ],
                },
            )]),
        }
    }

    #[test]
    fn addresses_are_built_from_names_and_ports() {
        let certified = HashSet::from([("shop.test".to_string(), 443)]);
        let summary = Summary::of(&manifest("https://shop.test/cart"), &certified);
        let shop = &summary.environments[0];
        assert_eq!(shop.entrypoint.as_deref(), Some("https://shop.test/cart"));
        assert_eq!(shop.services[0].url, "https://shop.test");
        assert_eq!(
            shop.services[0].sha256.as_deref(),
            Some("ab".repeat(32).as_str())
        );
        assert!(shop.services[0].certified);
        assert_eq!(shop.services[1].url, "http://shop.test:5173");
        assert!(!shop.services[1].certified && shop.services[1].sha256.is_none());

        let urls = summary.urls();
        assert!(urls.contains("https://shop.test") && urls.contains("https://shop.test/cart"));
        assert!(!urls.contains("https://evil.example"));
    }

    #[test]
    fn an_entrypoint_that_is_not_a_shared_address_is_dropped() {
        for entrypoint in [
            "https://evil.example/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "http://shop.test:9999/",
        ] {
            let summary = Summary::of(&manifest(entrypoint), &HashSet::new());
            assert_eq!(summary.environments[0].entrypoint, None, "{entrypoint}");
        }
        let summary = Summary::of(&manifest("https://shop.test/"), &HashSet::new());
        assert_eq!(
            summary.environments[0].entrypoint.as_deref(),
            Some("https://shop.test")
        );
    }
}
