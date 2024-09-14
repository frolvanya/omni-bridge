use anyhow::{Context, Result};
use tokio_stream::StreamExt;

use alloy::{
    providers::{Provider, ProviderBuilder, WsConnect},
    rpc::types::{Filter, Log},
};

use crate::{config, utils};

pub async fn start_indexer(config: config::Config, redis_client: redis::Client) -> Result<()> {
    let mut redis_connection = redis_client.get_multiplexed_tokio_connection().await?;

    let http_provider = ProviderBuilder::new().on_http(
        config
            .mainnet
            .eth_rpc_http_url
            .parse()
            .context("Failed to parse ETH rpc provider as url")?,
    );

    let ws_provider = ProviderBuilder::new()
        .on_ws(WsConnect::new(config.mainnet.eth_rpc_ws_url.clone()))
        .await
        .context("Failed to initialize WS provider")?;

    let latest_block = http_provider.get_block_number().await?;
    let from_block =
        utils::redis::get_last_processed_block(&mut redis_connection, "eth_last_processed_block")
            .await
            .map_or_else(|| latest_block.saturating_sub(10_000), |block| block);

    let filter = Filter::new()
        .address(config.mainnet.bridge_token_factory_address)
        .event("Withdraw(string,address,uint256,string,address)");

    for current_block in (from_block..latest_block).step_by(10_000) {
        let logs = http_provider
            .get_logs(
                &filter
                    .clone()
                    .from_block(current_block)
                    .to_block(current_block + 10_000),
            )
            .await?;
        for log in logs {
            process_log(&config, &mut redis_connection, &log).await;
        }
    }

    let mut stream = ws_provider.subscribe_logs(&filter).await?.into_stream();
    while let Some(log) = stream.next().await {
        process_log(&config, &mut redis_connection, &log).await;
    }

    Ok(())
}

async fn process_log(
    config: &config::Config,
    redis_connection: &mut redis::aio::MultiplexedConnection,
    log: &Log,
) {
    if let Some(block_height) = log.block_number {
        utils::redis::update_last_processed_block(
            redis_connection,
            &config.redis.eth_last_processed_block,
            block_height,
        )
        .await;
    }

    if let Some(tx_hash) = log.transaction_hash {
        utils::redis::add_event(
            redis_connection,
            &config.redis.eth_withdraw_events,
            tx_hash.to_string(),
            log.clone(),
        )
        .await;
    }
}
