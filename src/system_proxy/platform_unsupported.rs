use super::{Settings, SystemProxyPlatform};
use anyhow::{Result, bail};

#[derive(Default)]
pub struct Platform;
impl SystemProxyPlatform for Platform {
    fn supported(&self) -> bool {
        false
    }
    fn supports_socks(&self) -> bool {
        false
    }
    fn enable(&self, settings: &Settings) -> Result<()> {
        touch(settings);
        bail!("system proxy is not supported on this platform")
    }
    fn disable(&self) -> Result<()> {
        bail!("system proxy is not supported on this platform")
    }
    fn matches(&self, settings: &Settings) -> Result<bool> {
        touch(settings);
        bail!("system proxy is not supported on this platform")
    }
}

fn touch(settings: &Settings) {
    for endpoint in [settings.http.as_ref(), settings.socks.as_ref()]
        .into_iter()
        .flatten()
    {
        let _ = (&endpoint.host, endpoint.port, &endpoint.address);
    }
}
