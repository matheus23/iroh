use std::sync::Arc;

#[allow(unused_imports)]
use anyhow::{anyhow, Result};
use clap::Parser;
use iroh_gateway::{bad_bits::BadBits, core::Core, metrics};
#[cfg(feature = "uds-gateway")]
use iroh_one::uds;
use iroh_one::{
    cli::Args,
    config::{Config, CONFIG_FILE_NAME, ENV_PREFIX},
};
use iroh_rpc_types::Addr;
use iroh_util::{iroh_config_path, make_config};
#[cfg(feature = "uds-gateway")]
use tempdir::TempDir;
use tokio::sync::RwLock;
use tracing::{debug, error};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();

    let cfg_path = iroh_config_path(CONFIG_FILE_NAME)?;
    let sources = vec![Some(cfg_path), args.cfg.clone()];
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

    #[cfg(unix)]
    {
        match iroh_util::increase_fd_limit() {
            Ok(soft) => debug!("NOFILE limit: soft = {}", soft),
            Err(err) => error!("Error increasing NOFILE limit: {}", err),
        }
    }

    let (store_rpc, p2p_rpc) = {
        let (store_recv, store_sender) = Addr::new_mem();
        config.rpc_client.store_addr = Some(store_sender);
        let store_rpc = iroh_one::mem_store::start(store_recv, config.clone().store).await?;

        let (p2p_recv, p2p_sender) = Addr::new_mem();
        config.rpc_client.p2p_addr = Some(p2p_sender);
        let p2p_rpc = iroh_one::mem_p2p::start(p2p_recv, config.clone().p2p).await?;
        (store_rpc, p2p_rpc)
    };

    #[cfg(not(feature = "uds-gateway"))]
    let (rpc_addr, gw_sender) = Addr::new_mem();
    #[cfg(not(feature = "uds-gateway"))]
    {
        config.rpc_client.gateway_addr = Some(gw_sender);
    }

    config.synchronize_subconfigs();

    config.metrics = metrics::metrics_config_with_compile_time_info(config.metrics);
    println!("{:#?}", config);

    let metrics_config = config.metrics.clone();

    #[cfg(feature = "uds-gateway")]
    let rpc_addr = config
        .gateway
        .server_rpc_addr()?
        .ok_or_else(|| anyhow!("missing gateway rpc addr"))?;

    let bad_bits = match config.gateway.denylist {
        true => Arc::new(Some(RwLock::new(BadBits::new()))),
        false => Arc::new(None),
    };

    let shared_state = Core::make_state(Arc::new(config.clone()), Arc::clone(&bad_bits)).await?;

    let handler = Core::new_with_state(rpc_addr, Arc::clone(&shared_state)).await?;

    let metrics_handle = iroh_metrics::MetricsHandle::new(metrics_config)
        .await
        .expect("failed to initialize metrics");
    let server = handler.server();
    println!("HTTP endpoint listening on {}", server.local_addr());
    let core_task = tokio::spawn(async move {
        server.await.unwrap();
    });

    #[cfg(feature = "uds-gateway")]
    let uds_server_task = {
        let mut path = TempDir::new("iroh")?.path().join("ipfsd.http");
        if let Some(uds_path) = config.gateway_uds_path {
            path = uds_path;
        } else {
            // Create the parent path when using the default value since it's likely
            // it won't exist yet.
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(&parent);
            }
        }

        tokio::spawn(async move {
            if let Some(uds_server) = uds::uds_server(shared_state, path) {
                if let Err(err) = uds_server.await {
                    tracing::error!("Failure in http uds handler: {}", err);
                }
            }
        })
    };

    iroh_util::block_until_sigint().await;

    store_rpc.abort();
    p2p_rpc.abort();
    #[cfg(feature = "uds-gateway")]
    uds_server_task.abort();
    core_task.abort();

    metrics_handle.shutdown();
    Ok(())
}
