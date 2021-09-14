use anyhow::{anyhow, bail, Result};
use bdk::bitcoin::util::bip32::ExtendedPrivKey;
use bdk::bitcoin::Network;
use hkdf::Hkdf;
use rand::Rng;
use sha2::Sha256;
use std::convert::TryInto;
use std::path::Path;

pub struct Seed([u8; 256]);

impl Seed {
    /// Initialize a [`Seed`] from a path.
    ///
    /// Fails if the file does not exist or it exists but would be overwritten.
    pub async fn initialize(seed_file: &Path, generate: bool) -> Result<Seed> {
        let exists = seed_file.exists();

        let seed = match (exists, generate) {
            (true, false) => Seed::read_from(seed_file).await?,
            (false, true) => {
                let seed = Seed::default();
                seed.write_to(seed_file).await?;

                seed
            }
            (true, true) => bail!("Refusing to overwrite seed at {}", seed_file.display()),
            (false, false) => bail!("Seed file at {} does not exist", seed_file.display()),
        };

        Ok(seed)
    }

    pub async fn read_from(path: &Path) -> Result<Self> {
        let bytes = tokio::fs::read(path).await?;

        let bytes = bytes
            .try_into()
            .map_err(|_| anyhow!("Bytes from seed file don't fit into array"))?;

        Ok(Seed(bytes))
    }

    pub async fn write_to(&self, path: &Path) -> Result<()> {
        if path.exists() {
            anyhow::bail!("Refusing to overwrite file at {}", path.display())
        }

        tokio::fs::write(path, &self.0).await?;

        Ok(())
    }

    pub fn derive_extended_priv_key(&self, network: Network) -> Result<ExtendedPrivKey> {
        let h = Hkdf::<Sha256>::new(None, &self.0);
        let mut okm = [0u8; 64];
        h.expand(b"BITCOIN_WALLET_SEED", &mut okm)
            .expect("okm array is of correct length");

        let ext_priv_key = ExtendedPrivKey::new_master(network, &okm)?;

        Ok(ext_priv_key)
    }
}

impl Default for Seed {
    fn default() -> Self {
        let mut seed = [0u8; 256];
        rand::thread_rng().fill(&mut seed);

        Self(seed)
    }
}
