use std::sync::Arc;

use anyhow::{anyhow, Result};
use clap::Parser;
use iroh_gateway::{
    bad_bits::{self, BadBits},
    cli::Args,
    config::{Config, CONFIG_FILE_NAME, ENV_PREFIX},
    core::Core,
    handlers::StateConfig,
    metrics,
};
use iroh_rpc_types::gateway::GatewayServerAddr;
use iroh_util::{iroh_home_path, make_config};
use tokio::sync::RwLock;
use tracing::{debug, error};

async fn serve(_: usize, config: Config, rpc_addr: GatewayServerAddr) {
    let handler = Core::new(Arc::new(config), Some(rpc_addr), Arc::new(None))
        .await
        .unwrap();
    let server = handler.server();
    println!("listening on {}", server.local_addr());
    server.await.unwrap();
}

// #[tokio::main(flavor = "multi_thread")]
fn main() -> Result<()> {
    // let bad_bits_handle = bad_bits::spawn_bad_bits_updater(Arc::clone(&bad_bits));

    // let metrics_handle = iroh_metrics::MetricsHandle::new(metrics_config)
    //     .await
    //     .expect("failed to initialize metrics");

    #[cfg(unix)]
    {
        match iroh_util::increase_fd_limit() {
            Ok(soft) => debug!("NOFILE limit: soft = {}", soft),
            Err(err) => error!("Error increasing NOFILE limit: {}", err),
        }
    }

    let mut handlers = Vec::new();
    for i in 0..num_cpus::get() {
        let h = std::thread::spawn(move || {
            let args = Args::parse();
            let sources = vec![iroh_home_path(CONFIG_FILE_NAME), args.cfg.clone()];
            let mut config = make_config(
                // default
                Config::default(),
                // potential config files
                sources,
                // env var prefix for this config
                ENV_PREFIX,
                // map of present command line arguments
                args.make_overrides_map(),
            )
            .unwrap();
            config.metrics = metrics::metrics_config_with_compile_time_info(config.metrics);
            println!("{:#?}", config);

            let metrics_config = config.metrics.clone();
            let bad_bits = match config.denylist {
                true => Arc::new(Some(RwLock::new(BadBits::new()))),
                false => Arc::new(None),
            };
            let rpc_addr = match config.server_rpc_addr().unwrap() {
                Some(addr) => addr,
                None => panic!("server_rpc_addr not set"),
            };

            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(serve(i, config.clone(), rpc_addr));
        });
        handlers.push(h);
    }

    for h in handlers {
        h.join().unwrap();
    }

    // iroh_util::block_until_sigint().await;
    // core_task.abort();

    // metrics_handle.shutdown();
    // if let Some(handle) = bad_bits_handle {
    //     handle.abort();
    // }

    Ok(())
}
