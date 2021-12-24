use anyhow::bail;
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::util::bip32::ExtendedPrivKey;
use bdk::bitcoin::Network;
use bip39::Language;
use bip39::Mnemonic;
use rand::thread_rng;
use std::path::Path;
use std::str::FromStr;

#[async_trait]
pub trait MnemonicExt {
    /// Initialize a [`WalletSeed`] from a path.
    /// Generates new seed if there was no seed found in the given path.
    async fn initialize(seed_file: &Path) -> Result<Self>
    where
        Self: Sized;
    /// Reinitialize an existing [`WalletSeed`].
    /// Overwrites if a seed was found in the given path.
    async fn reinitialize(seed_file: &Path) -> Result<Self>
    where
        Self: Sized;
    /// Restore a [`WalletSeed`] from a mnemonic.
    /// Overwrites if a seed was found in the given path.
    async fn restore(seed_file: &Path, mnemonic: Mnemonic) -> Result<()>;
    async fn read_from(path: &Path) -> Result<Self>
    where
        Self: Sized;
    async fn write_to(&self, path: &Path) -> Result<()>;
    fn derive_extended_priv_key(&self, network: Network) -> Result<ExtendedPrivKey>;
}

#[async_trait]
impl MnemonicExt for Mnemonic {
    async fn initialize(seed_file: &Path) -> Result<Self> {
        let seed = if !seed_file.exists() {
            tracing::info!("No wallet seed found. Generating new wallet seed");
            let seed = generate_random_mnemonic();
            seed.write_to(seed_file).await?;
            seed
        } else {
            Self::read_from(seed_file).await?
        };
        Ok(seed)
    }

    async fn reinitialize(seed_file: &Path) -> Result<Mnemonic> {
        let mnemonic = generate_random_mnemonic();
        Self::restore(seed_file, mnemonic.clone()).await?;
        Ok(mnemonic)
    }

    async fn restore(seed_file: &Path, mnemonic: Mnemonic) -> Result<()> {
        tokio::fs::remove_file(seed_file).await?;

        if seed_file.exists() {
            bail!("Failed restore wallet from mnemonic. Old wallet seed file was not successfully deleted.");
        }

        mnemonic.write_to(seed_file).await?;

        Ok(())
    }

    async fn read_from(path: &Path) -> Result<Self> {
        let bytes = tokio::fs::read(path).await?;
        let mnemonic = Mnemonic::from_str(&String::from_utf8(bytes)?)?;

        Ok(mnemonic)
    }

    async fn write_to(&self, path: &Path) -> Result<()> {
        tokio::fs::write(path, self.to_string().as_bytes()).await?;

        Ok(())
    }

    fn derive_extended_priv_key(&self, network: Network) -> Result<ExtendedPrivKey> {
        let ext_priv_key = ExtendedPrivKey::new_master(network, &self.to_seed(""))?;

        Ok(ext_priv_key)
    }
}

fn generate_random_mnemonic() -> Mnemonic {
    Mnemonic::generate_in_with(&mut thread_rng(), Language::English, 24)
        .expect("Randomly generated a 24 word english mnemonic phrase")
}
