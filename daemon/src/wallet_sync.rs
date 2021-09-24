use crate::wallet::Wallet;
use crate::WalletInfo;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::sleep;

pub async fn new(wallet: Wallet, sender: watch::Sender<WalletInfo>) {
    loop {
        sleep(Duration::from_secs(10)).await;

        let info = match wallet.sync().await {
            Ok(info) => info,
            Err(e) => {
                tracing::warn!("Failed to sync wallet: {:#}", e);
                continue;
            }
        };

        if sender.send(info).is_err() {
            tracing::warn!("Wallet feed receiver not available, stopping wallet sync");
            break;
        }
    }
}
