mod bridge;
mod config;
mod error;
mod events;
mod nfc;
#[cfg(windows)]
mod registry_sounds;
#[cfg(windows)]
mod service;
mod ws;

use clap::Parser;
use config::AppArgs;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = AppArgs::parse();

    if args.console {
        run_console(args)
    } else {
        #[cfg(windows)]
        {
            // Default: run as Windows Service
            service::run_service().map_err(|e| {
                eprintln!("Failed to start as service: {e}");
                eprintln!("Hint: Use --console flag to run in console mode");
                Box::new(e) as Box<dyn std::error::Error>
            })
        }
        #[cfg(not(windows))]
        {
            // On non-Windows, always run in console mode
            run_console(args)
        }
    }
}

fn run_console(args: AppArgs) -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nfc_bridge=info".into()),
        )
        .init();

    let config = config::Config::from_args_and_file(&args)?;

    // Disable device sounds for console mode
    #[cfg(windows)]
    let _sound_guard = crate::registry_sounds::SoundSuppressor::new();
    #[cfg(windows)]
    if _sound_guard.is_none() {
        tracing::warn!("Failed to disable device sounds (registry access denied)");
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let shutdown = tokio_util::sync::CancellationToken::new();
        let shutdown_trigger = shutdown.clone();

        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            shutdown_trigger.cancel();
        });

        bridge::run(config, shutdown).await
    })?;

    Ok(())
}
