mod assets;
mod cli;
mod core;
mod server;
mod system_proxy;

use std::{net::SocketAddr, path::Path};

use anyhow::{Context, Result};
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let options = cli::Options::parse(std::env::args_os().skip(1))?;
    let executable = std::env::current_exe().context("resolve WebUI executable")?;
    let core = core::resolve_binary(&options.core_binary, options.external_core, &executable)?;

    if core.managed {
        if let Some(warning) = cli::managed_listen_warning(&options.listen) {
            warn!("{warning}");
        }
    } else if !options.external_core {
        warn!(core = %options.core_address, "bundled Core was not found; using external Core");
    }

    let core_address = if core.managed {
        format!("http://{}", options.core_listen)
    } else {
        options.core_address.clone()
    };
    let listen: SocketAddr = options
        .listen
        .parse()
        .with_context(|| format!("invalid WebUI listen address {:?}", options.listen))?;
    let supervisor = if let Some(binary) = core.path {
        let data = if options.data_directory.is_empty() {
            executable.parent().unwrap_or(Path::new(".")).join("data")
        } else {
            options.data_directory.clone().into()
        };
        let paths = core::ManagedPaths::resolve(
            &data,
            (!options.config_file.is_empty()).then(|| Path::new(&options.config_file)),
            (!options.resume_file.is_empty()).then(|| Path::new(&options.resume_file)),
        )?;
        let managed_config = core::ManagedConfig {
            paths,
            core_listen: options.core_listen.clone(),
        };
        let supervisor = core::Supervisor::new(
            binary,
            managed_config,
            core_address.clone(),
            options.core_log_console,
        );
        supervisor.start().await?;
        Some(supervisor)
    } else {
        None
    };
    let system_proxy = system_proxy::Controller::new();
    let app = server::router(
        core_address.parse().context("invalid Core address")?,
        supervisor.clone(),
        system_proxy.clone(),
    )?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("listen on {}", options.listen))?;

    info!(
        "Sumire listening on http://{} (managed: {})",
        options.listen,
        supervisor.is_some()
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve WebUI")?;
    if let Err(error) = system_proxy.close().await {
        warn!(%error, "disable system proxy");
    }
    if let Some(supervisor) = supervisor {
        if let Err(error) = supervisor.stop().await {
            warn!(%error, "stop managed Core");
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    let interrupt = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = interrupt => {},
        _ = terminate => {},
    }
}
