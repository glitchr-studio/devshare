//! Makes the device ask the tunnel's resolver for the shared hostnames, and
//! only for them, when this process runs as root. Everything installed here
//! is removed on drop, and anything a killed process left behind is removed
//! at the next start. Without root, the privileged helper does the same (see
//! [`super::helper`]), with the same code: `devshare_protocol::system_dns`.

use anyhow::Result;

use super::AddressPlan;

pub struct SystemDns {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    installed: devshare_protocol::system_dns::Installed,
}

impl SystemDns {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn install(interface: &str, plan: &AddressPlan) -> Result<Self> {
        let installed = devshare_protocol::system_dns::install(
            interface,
            plan.names(),
            AddressPlan::resolver(),
        )?;
        Ok(Self { installed })
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn install(_interface: &str, _plan: &AddressPlan) -> Result<Self> {
        anyhow::bail!("joining a session is not supported on this system yet")
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for SystemDns {
    fn drop(&mut self) {
        if let Err(error) = devshare_protocol::system_dns::remove(&self.installed) {
            tracing::error!("could not remove the session's names: {error}");
        }
    }
}
