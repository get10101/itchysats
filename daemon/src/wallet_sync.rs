use crate::model::WalletInfo;
use crate::wallet;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::sleep;
use xtra::Address;

pub async fn new(wallet: Address<wallet::Actor>, sender: watch::Sender<WalletInfo>) {
    loop {
        sleep(Duration::from_secs(10)).await;

        let info = match wallet
            .send(wallet::Sync)
            .await
            .expect("Wallet actor to be available")
        {
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
