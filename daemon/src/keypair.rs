use bdk::bitcoin;
use bdk::bitcoin::secp256k1;
use bdk::bitcoin::secp256k1::SECP256K1;
use rand::CryptoRng;
use rand::RngCore;

pub fn new<R>(rng: &mut R) -> (secp256k1::SecretKey, bitcoin::PublicKey)
where
    R: RngCore + CryptoRng,
{
    let sk = secp256k1::SecretKey::new(rng);
    let pk = bitcoin::PublicKey::new(secp256k1::PublicKey::from_secret_key(SECP256K1, &sk));

    (sk, pk)
}
