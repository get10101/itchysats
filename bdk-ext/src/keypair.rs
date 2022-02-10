use crate::SecretKeyExt;
use bdk::bitcoin;
use bdk::bitcoin::secp256k1;
use rand::CryptoRng;
use rand::RngCore;

pub fn new<R>(rng: &mut R) -> (secp256k1::SecretKey, bitcoin::PublicKey)
where
    R: RngCore + CryptoRng,
{
    let sk = secp256k1::SecretKey::new(rng);
    let pk = bitcoin::PublicKey::new(sk.to_public_key());

    (sk, pk)
}
