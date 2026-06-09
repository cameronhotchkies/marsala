pub mod cli;
pub mod config;
pub mod event_log;
pub mod http;
pub mod openai;
pub mod proxy;

use std::{future::IntoFuture, sync::Once};

use anyhow::{Context, Result};
use axum::serve;
use clap::Parser;
use cli::{Cli, Command, ConfigCommand, LogsCommand};
use config::AppConfig;
use event_log::EventLogWriter;
use tokio::{net::TcpListener, sync::watch};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

static TRACING: Once = Once::new();

pub async fn run() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let config = AppConfig::load(cli.config_path.as_deref())?;

    match cli.command.unwrap_or_default() {
        Command::Serve => serve_command(config).await,
        Command::Config(args) => match args.command {
            ConfigCommand::Print(args) => {
                let rendered = if args.all {
                    config.to_toml_string()?
                } else {
                    config.to_active_toml_string()?
                };
                println!("{rendered}");
                Ok(())
            }
        },
        Command::Logs(args) => match args.command {
            LogsCommand::Tail(args) => {
                event_log::tail_log_file(&config.logging.path, args.lines, args.follow).await
            }
        },
    }
}

fn init_tracing() {
    TRACING.call_once(|| {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info,marsala=info"));

        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .compact()
            .init();
    });
}

async fn serve_command(config: AppConfig) -> Result<()> {
    let bind_addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind {bind_addr}"))?;
    let proxy_listener = if config.proxy.enabled {
        let proxy_bind_addr = format!("{}:{}", config.proxy.host, config.proxy.port);
        Some((
            proxy_bind_addr.clone(),
            TcpListener::bind(&proxy_bind_addr)
                .await
                .with_context(|| format!("failed to bind proxy listener {proxy_bind_addr}"))?,
        ))
    } else {
        None
    };

    let local_addr = listener.local_addr().context("missing local address")?;
    let mut event_writer =
        EventLogWriter::spawn(&config.logging.path, config.logging.enabled).await?;
    let event_log = event_writer.handle();

    event_log.emit(
        "service_started",
        serde_json::json!({
            "bind_addr": bind_addr,
            "local_addr": local_addr.to_string(),
        }),
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let proxy_task = if let Some((proxy_bind_addr, proxy_listener)) = proxy_listener {
        let proxy_local_addr = proxy_listener
            .local_addr()
            .context("missing proxy listener local address")?;
        event_log.emit(
            "proxy_listener_started",
            serde_json::json!({
                "bind_addr": proxy_bind_addr,
                "local_addr": proxy_local_addr.to_string(),
            }),
        );
        info!(address = %proxy_local_addr, "marsala proxy probe listening");
        Some(tokio::spawn(proxy::serve_listener(
            proxy_listener,
            event_log.clone(),
            shutdown_rx.clone(),
        )))
    } else {
        None
    };

    let app = http::build_router(config.clone(), event_log.clone())?;
    info!(address = %local_addr, "marsala listening");

    let shutdown_log = event_log.clone();
    let mut server_shutdown_rx = shutdown_rx.clone();
    let server = serve(listener, app).with_graceful_shutdown(async move {
        while server_shutdown_rx.changed().await.is_ok() {
            if *server_shutdown_rx.borrow() {
                break;
            }
        }
    });

    let mut server = Box::pin(server.into_future());
    let result = tokio::select! {
        result = &mut server => {
            let _ = shutdown_tx.send(true);
            result
        }
        signal = tokio::signal::ctrl_c() => {
            if signal.is_ok() {
                info!("shutdown requested");
                shutdown_log.emit(
                    "shutdown_requested",
                    serde_json::json!({ "signal": "ctrl_c" }),
                );
            }
            let _ = shutdown_tx.send(true);
            server.await
        }
    };

    if let Some(proxy_task) = proxy_task {
        match proxy_task.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                warn!(%error, "proxy probe listener exited with error");
                if result.is_ok() {
                    return Err(error);
                }
            }
            Err(error) => {
                warn!(%error, "proxy probe listener task panicked");
            }
        }
    }

    let reason = if result.is_ok() {
        "graceful_shutdown"
    } else {
        "server_error"
    };

    event_log.emit("service_stopped", serde_json::json!({ "reason": reason }));
    drop(event_log);
    event_writer.shutdown().await?;

    if let Err(error) = result {
        error!(%error, "server exited with error");
        return Err(error).context("axum server failed");
    }

    Ok(())
}
