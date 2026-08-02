#[cfg(target_os = "macos")]
mod platform_darwin;
#[cfg(not(any(target_os = "macos", windows)))]
mod platform_unsupported;
#[cfg(windows)]
mod platform_windows;

use std::{net::IpAddr, sync::Arc, time::Duration};

use anyhow::{Result, bail};
use serde::Serialize;
use tokio::sync::Mutex;

#[cfg(target_os = "macos")]
use platform_darwin::Platform;
#[cfg(not(any(target_os = "macos", windows)))]
use platform_unsupported::Platform;
#[cfg(windows)]
use platform_windows::Platform;

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub address: String,
}

#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub http: Option<Endpoint>,
    pub socks: Option<Endpoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct State {
    pub supported: bool,
    pub socks_supported: bool,
    pub enabled: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub http_address: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub socks_address: String,
}

pub trait SystemProxyPlatform: Send + Sync + 'static {
    fn supported(&self) -> bool;
    fn supports_socks(&self) -> bool;
    fn enable(&self, settings: &Settings) -> Result<()>;
    fn disable(&self) -> Result<()>;
    fn matches(&self, settings: &Settings) -> Result<bool>;
}

#[derive(Default)]
struct Inner {
    enabled: bool,
    http_address: String,
    socks_address: String,
    settings: Settings,
    guard_started: bool,
    guard_error: String,
}

pub struct Controller {
    platform: Arc<dyn SystemProxyPlatform>,
    inner: Mutex<Inner>,
    guard_interval: Duration,
}

impl Controller {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            platform: Arc::new(Platform),
            inner: Mutex::new(Inner::default()),
            guard_interval: Duration::from_secs(5),
        })
    }

    #[cfg(test)]
    fn with_platform(
        platform: Arc<dyn SystemProxyPlatform>,
        guard_interval: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            platform,
            inner: Mutex::new(Inner::default()),
            guard_interval,
        })
    }

    pub async fn status(&self) -> State {
        let inner = self.inner.lock().await;
        self.state(&inner)
    }

    pub async fn configure(
        self: &Arc<Self>,
        enabled: bool,
        http: &str,
        socks: &str,
    ) -> Result<State> {
        let mut inner = self.inner.lock().await;
        if !self.platform.supported() {
            bail!("system proxy is not supported on this platform");
        }
        if !enabled {
            self.platform.disable()?;
            inner.enabled = false;
            inner.http_address.clear();
            inner.socks_address.clear();
            inner.settings = Settings::default();
            inner.guard_error.clear();
            return Ok(self.state(&inner));
        }

        let settings = Settings {
            http: (!http.trim().is_empty())
                .then(|| parse_endpoint(http))
                .transpose()?,
            socks: (self.platform.supports_socks() && !socks.trim().is_empty())
                .then(|| parse_endpoint(socks))
                .transpose()?,
        };
        if settings.http.is_none() && settings.socks.is_none() {
            bail!("no supported active proxy inbound is available");
        }
        self.platform.enable(&settings)?;
        inner.enabled = true;
        inner.http_address = settings
            .http
            .as_ref()
            .map_or(String::new(), |endpoint| endpoint.address.clone());
        inner.socks_address = settings
            .socks
            .as_ref()
            .map_or(String::new(), |endpoint| endpoint.address.clone());
        inner.settings = settings;
        if !inner.guard_started {
            inner.guard_started = true;
            spawn_guard(Arc::downgrade(self), self.guard_interval);
        }
        Ok(self.state(&inner))
    }

    pub async fn close(&self) -> Result<()> {
        let mut inner = self.inner.lock().await;
        if inner.enabled && self.platform.supported() {
            self.platform.disable()?;
            inner.enabled = false;
            inner.http_address.clear();
            inner.socks_address.clear();
            inner.settings = Settings::default();
        }
        Ok(())
    }

    fn state(&self, inner: &Inner) -> State {
        State {
            supported: self.platform.supported(),
            socks_supported: self.platform.supports_socks(),
            enabled: inner.enabled,
            http_address: inner.http_address.clone(),
            socks_address: inner.socks_address.clone(),
        }
    }

    async fn enforce(&self) {
        let mut inner = self.inner.lock().await;
        if !inner.enabled {
            return;
        }
        let result = self.platform.matches(&inner.settings).and_then(|matches| {
            if matches {
                Ok(false)
            } else {
                self.platform.enable(&inner.settings).map(|_| true)
            }
        });
        match result {
            Ok(reapplied) => {
                if reapplied {
                    tracing::info!("system proxy settings changed externally; reapplied");
                }
                inner.guard_error.clear();
            }
            Err(error) => {
                let message = error.to_string();
                if message != inner.guard_error {
                    tracing::error!(%error, "enforce system proxy");
                }
                inner.guard_error = message;
            }
        }
    }
}

fn spawn_guard(controller: std::sync::Weak<Controller>, guard_interval: Duration) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(guard_interval);
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(controller) = controller.upgrade() else {
                break;
            };
            controller.enforce().await;
        }
    });
}

pub fn parse_endpoint(address: &str) -> Result<Endpoint> {
    let address = address.trim();
    let (host, port) = split_host_port(address)?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !loopback {
        bail!("system proxy address must use a loopback host");
    }
    let port: u16 = port
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid proxy port {port:?}"))?;
    if port == 0 {
        bail!("invalid proxy port {port:?}");
    }
    let normalized = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    Ok(Endpoint {
        host: host.to_owned(),
        port,
        address: normalized,
    })
}

fn split_host_port(address: &str) -> Result<(&str, &str)> {
    if let Some(rest) = address.strip_prefix('[') {
        let (host, port) = rest
            .split_once("]:")
            .ok_or_else(|| anyhow::anyhow!("invalid proxy listen address {address:?}"))?;
        return Ok((host, port));
    }
    address
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid proxy listen address {address:?}"))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use super::*;

    #[derive(Default)]
    struct FakePlatform {
        supported: bool,
        socks: bool,
        matches: AtomicBool,
        enable_calls: AtomicUsize,
        disable_calls: AtomicUsize,
        fail_disable: AtomicBool,
        settings: StdMutex<Settings>,
    }

    impl SystemProxyPlatform for FakePlatform {
        fn supported(&self) -> bool {
            self.supported
        }
        fn supports_socks(&self) -> bool {
            self.socks
        }
        fn enable(&self, settings: &Settings) -> Result<()> {
            self.enable_calls.fetch_add(1, Ordering::SeqCst);
            *self.settings.lock().unwrap() = settings.clone();
            Ok(())
        }
        fn disable(&self) -> Result<()> {
            self.disable_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_disable.load(Ordering::SeqCst) {
                bail!("disable failed");
            }
            Ok(())
        }
        fn matches(&self, _: &Settings) -> Result<bool> {
            Ok(self.matches.load(Ordering::SeqCst))
        }
    }

    #[test]
    fn parses_loopback_addresses() {
        assert_eq!(parse_endpoint("127.0.0.1:1081").unwrap().port, 1081);
        assert_eq!(parse_endpoint("[::1]:1080").unwrap().host, "::1");
        assert!(parse_endpoint("192.168.1.2:1081").is_err());
        assert!(parse_endpoint("127.0.0.1:0").is_err());
    }

    #[tokio::test]
    async fn configures_and_closes_proxy() {
        let platform = Arc::new(FakePlatform {
            supported: true,
            socks: true,
            ..Default::default()
        });
        let controller = Controller::with_platform(platform.clone(), Duration::from_secs(60));
        let state = controller
            .configure(true, "127.0.0.1:1081", "127.0.0.1:1080")
            .await
            .unwrap();
        assert!(state.enabled);
        assert_eq!(state.http_address, "127.0.0.1:1081");
        assert_eq!(state.socks_address, "127.0.0.1:1080");
        controller.close().await.unwrap();
        assert!(!controller.status().await.enabled);
        assert_eq!(platform.disable_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_disable_keeps_enabled_state() {
        let platform = Arc::new(FakePlatform {
            supported: true,
            fail_disable: AtomicBool::new(true),
            ..Default::default()
        });
        let controller = Controller::with_platform(platform.clone(), Duration::from_secs(60));
        controller
            .configure(true, "127.0.0.1:1081", "")
            .await
            .unwrap();
        assert!(controller.configure(false, "", "").await.is_err());
        assert!(controller.status().await.enabled);
        platform.fail_disable.store(false, Ordering::SeqCst);
        controller.close().await.unwrap();
    }

    #[tokio::test]
    async fn guard_reapplies_changed_settings() {
        let platform = Arc::new(FakePlatform {
            supported: true,
            ..Default::default()
        });
        let controller = Controller::with_platform(platform.clone(), Duration::from_millis(10));
        controller
            .configure(true, "127.0.0.1:1081", "")
            .await
            .unwrap();
        for _ in 0..100 {
            if platform.enable_calls.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(platform.enable_calls.load(Ordering::SeqCst) >= 2);
        platform.matches.store(true, Ordering::SeqCst);
        controller.close().await.unwrap();
    }
}
