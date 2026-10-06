//! The addresses of a session on the guest: one per shared hostname, valid
//! only on this device and only while the tunnel is up.

use std::{collections::HashMap, net::Ipv4Addr};

use anyhow::{bail, Result};
use devshare_protocol::{normalize_host, Manifest};

/// `100.90.0.0/24`: the tunnel's own address, the resolver, then the names.
const NETWORK: [u8; 3] = [100, 90, 0];
const FIRST_NAME: u8 = 10;
const LAST_NAME: u8 = 250;

#[derive(Debug, Clone)]
pub struct AddressPlan {
    names: Vec<(String, Ipv4Addr)>,
    by_name: HashMap<String, Ipv4Addr>,
    by_address: HashMap<Ipv4Addr, String>,
    ports: HashMap<String, Vec<u16>>,
}

impl AddressPlan {
    pub fn new(manifest: &Manifest) -> Result<Self> {
        let hostnames = manifest.hostnames();
        if hostnames.len() > (LAST_NAME - FIRST_NAME) as usize + 1 {
            bail!(
                "the session shares {} hostnames, more than a guest can map",
                hostnames.len()
            );
        }

        let names: Vec<(String, Ipv4Addr)> = hostnames
            .into_iter()
            .zip(FIRST_NAME..=LAST_NAME)
            .map(|(name, last)| (name, address(last)))
            .collect();

        let mut ports: HashMap<String, Vec<u16>> = HashMap::new();
        for service in manifest.environments.values().flat_map(|env| &env.services) {
            ports
                .entry(normalize_host(&service.host))
                .or_default()
                .push(service.port);
        }

        Ok(Self {
            by_name: names.iter().cloned().collect(),
            by_address: names.iter().map(|(name, ip)| (*ip, name.clone())).collect(),
            names,
            ports,
        })
    }

    pub const fn local() -> Ipv4Addr {
        address(1)
    }

    pub const fn resolver() -> Ipv4Addr {
        address(2)
    }

    pub const fn netmask() -> Ipv4Addr {
        Ipv4Addr::new(255, 255, 255, 0)
    }

    /// The session's network, to route into the tunnel.
    pub const fn network() -> (Ipv4Addr, u8) {
        (address(0), 24)
    }

    pub fn names(&self) -> &[(String, Ipv4Addr)] {
        &self.names
    }

    pub fn address_of(&self, name: &str) -> Option<Ipv4Addr> {
        self.by_name.get(&normalize_host(name)).copied()
    }

    pub fn name_of(&self, address: Ipv4Addr) -> Option<&str> {
        self.by_address.get(&address).map(String::as_str)
    }

    /// Whether the manifest lists this port. The host decides in the end;
    /// this only spares it the streams it would refuse.
    pub fn shares(&self, name: &str, port: u16) -> bool {
        self.ports
            .get(name)
            .is_some_and(|ports| ports.contains(&port))
    }
}

const fn address(last: u8) -> Ipv4Addr {
    Ipv4Addr::new(NETWORK[0], NETWORK[1], NETWORK[2], last)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;

    use devshare_protocol::{Environment, Service, SessionInfo, Transport};

    use super::*;

    pub(crate) fn manifest(services: &[(&str, u16)]) -> Manifest {
        let services: Vec<Service> = services
            .iter()
            .map(|(host, port)| Service {
                host: host.to_string(),
                port: *port,
                protocol: Transport::Tcp,
                tls: None,
            })
            .collect();
        let dns = services
            .iter()
            .map(|service| service.host.clone())
            .collect();
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
                    dns,
                    services,
                },
            )]),
        }
    }

    #[test]
    fn one_address_per_hostname_whatever_the_number_of_ports() {
        let plan = AddressPlan::new(&manifest(&[
            ("shop.test", 443),
            ("shop.test", 5173),
            ("API.shop.test", 8080),
        ]))
        .unwrap();

        assert_eq!(plan.names().len(), 2);
        let shop = plan.address_of("Shop.Test.").unwrap();
        assert_eq!(plan.name_of(shop), Some("shop.test"));
        assert_ne!(plan.address_of("api.shop.test"), Some(shop));
        assert!(plan.shares("shop.test", 5173));
        assert!(!plan.shares("shop.test", 22));
        assert_eq!(plan.address_of("other.test"), None);
    }
}
