use anyhow::anyhow;
use anyhow::Result;
use bdk::bitcoin::util::bip32::ExtendedPrivKey;
use bdk::bitcoin::Network;
use bip39::Mnemonic;
use hkdf::Hkdf;
use rand::Rng;
use sha2::Sha256;
use std::convert::TryInto;
use std::path::Path;

#[derive(Copy, Clone)]
pub struct Seed([u8; 256]);

impl Seed {
    /// Initialize a [`Seed`] from a path.
    /// Generates new seed if there was no seed found in the given path
    pub async fn initialize(seed_file: &Path) -> Result<Seed> {
        let seed = if !seed_file.exists() {
            tracing::info!("No seed found. Generating new seed");
            let seed = Seed::default();
            seed.write_to(seed_file).await?;
            seed
        } else {
            Seed::read_from(seed_file).await?
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
        let mut ext_priv_key_seed = [0u8; 64];

        Hkdf::<Sha256>::new(None, &self.0)
            .expand(b"BITCOIN_WALLET_SEED", &mut ext_priv_key_seed)
            .expect("okm array is of correct length");

        let ext_priv_key = ExtendedPrivKey::new_master(network, &ext_priv_key_seed)?;

        Ok(ext_priv_key)
    }

    pub fn derive_auth_password<P: From<[u8; 32]>>(&self) -> P {
        let mut password = [0u8; 32];

        Hkdf::<Sha256>::new(None, &self.0)
            .expand(b"HTTP_AUTH_PASSWORD", &mut password)
            .expect("okm array is of correct length");

        P::from(password)
    }

    pub fn derive_identity(&self) -> (x25519_dalek::PublicKey, x25519_dalek::StaticSecret) {
        let mut secret = [0u8; 32];

        Hkdf::<Sha256>::new(None, &self.0)
            .expand(b"NOISE_STATIC_SECRET", &mut secret)
            .expect("okm array is of correct length");

        let identity_sk = x25519_dalek::StaticSecret::from(secret);
        (x25519_dalek::PublicKey::from(&identity_sk), identity_sk)
    }
}

impl Default for Seed {
    fn default() -> Self {
        let mut seed = [0u8; 256];
        rand::thread_rng().fill(&mut seed);

        Self(seed)
    }
}

impl TryFrom<Mnemonic> for Seed {
    type Error = anyhow::Error;

    fn try_from(mnemonic: Mnemonic) -> Result<Self> {
        let entropy = <[u8; 256]>::try_from(mnemonic.to_entropy().as_slice())?;
        Ok(Self { 0: entropy })
    }
}

impl TryInto<Mnemonic> for Seed {
    type Error = anyhow::Error;

    fn try_into(self) -> Result<Mnemonic> {
        Ok(Mnemonic::from_entropy(&self.0)?)
    }
}
