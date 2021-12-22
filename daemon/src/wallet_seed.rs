use anyhow::bail;
use anyhow::Result;
use bdk::bitcoin::util::bip32::ExtendedPrivKey;
use bdk::bitcoin::Network;
use bip39::Language;
use bip39::Mnemonic;
use rand::thread_rng;
use std::path::Path;
use std::str::FromStr;

#[derive(Clone)]
pub struct WalletSeed(Mnemonic);

impl WalletSeed {
    pub fn new(mnemonic: Mnemonic) -> Self {
        Self(mnemonic)
    }

    /// Initialize a [`WalletSeed`] from a path.
    /// Generates new seed if there was no seed found in the given path
    pub async fn initialize(seed_file: &Path) -> Result<Self> {
        let seed = if !seed_file.exists() {
            tracing::info!("No wallet seed found. Generating new wallet seed");
            let seed = Self::default();
            seed.write_to(seed_file).await?;
            seed
        } else {
            Self::read_from(seed_file).await?
        };
        Ok(seed)
    }

    pub async fn reinitialize(seed_file: &Path) -> Result<Self> {
        tokio::fs::remove_file(seed_file).await?;

        if seed_file.exists() {
            bail!("Failed to reinitialize wallet seed file. Old wallet seed file was not successfully deleted.");
        }

        let seed = Self::default();
        seed.write_to(seed_file).await?;

        Ok(seed)
    }

    pub async fn restore(seed_file: &Path, mnemonic: Mnemonic) -> Result<Self> {
        tokio::fs::remove_file(seed_file).await?;

        if seed_file.exists() {
            bail!("Failed restore wallet from mnemonic. Old wallet seed file was not successfully deleted.");
        }

        let seed = Self::new(mnemonic);
        seed.write_to(seed_file).await?;

        Ok(seed)
    }

    pub fn mnemonic(&self) -> Mnemonic {
        self.0.clone()
    }

    pub async fn read_from(path: &Path) -> Result<Self> {
        let bytes = tokio::fs::read(path).await?;
        let mnemonic = Mnemonic::from_str(&String::from_utf8(bytes)?)?;

        Ok(Self(mnemonic))
    }

    pub async fn write_to(&self, path: &Path) -> Result<()> {
        tokio::fs::write(path, self.0.to_string().as_bytes()).await?;

        Ok(())
    }

    pub fn derive_extended_priv_key(&self, network: Network) -> Result<ExtendedPrivKey> {
        let ext_priv_key = ExtendedPrivKey::new_master(network, &self.0.to_seed(""))?;

        Ok(ext_priv_key)
    }
}

impl Default for WalletSeed {
    fn default() -> Self {
        let mnemonic = Mnemonic::generate_in_with(&mut thread_rng(), Language::English, 24)
            .expect("Randomly generated a 24 word english mnemonic phrase");
        Self(mnemonic)
    }
}
