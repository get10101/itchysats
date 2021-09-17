pub use secp256k1_zkp::*;

use bdk::bitcoin::hashes::Hash;
use bip340_hash::Bip340Hash;
use rand::{CryptoRng, RngCore};
use secp256k1_zkp::bitcoin_hashes::sha256;
use secp256k1_zkp::{schnorrsig, SecretKey};

mod bip340_hash;
mod secp_utils;

/// Sign `msg` with the oracle's `key_pair` and a pre-computed `nonce`
/// whose corresponding public key was included in a previous
/// announcement.
pub fn attest(
    key_pair: &schnorrsig::KeyPair,
    nonce: &SecretKey,
    msg: &[u8],
) -> schnorrsig::Signature {
    let msg = secp256k1_zkp::Message::from_hashed_data::<sha256::Hash>(msg);
    secp_utils::schnorr_sign_with_nonce(&msg, key_pair, nonce)
}

pub fn nonce<R>(rng: &mut R) -> (SecretKey, schnorrsig::PublicKey)
where
    R: RngCore + CryptoRng,
{
    let nonce = SecretKey::new(rng);

    let key_pair = schnorrsig::KeyPair::from_secret_key(SECP256K1, nonce);
    let nonce_pk = schnorrsig::PublicKey::from_keypair(SECP256K1, &key_pair);

    (nonce, nonce_pk)
}

pub fn msg_hash(
    pk: &schnorrsig::PublicKey,
    nonce_pk: &schnorrsig::PublicKey,
    msg: &[u8],
) -> Vec<u8> {
    let mut buf = Vec::<u8>::new();
    buf.extend(&nonce_pk.serialize());
    buf.extend(&pk.serialize());
    buf.extend(
        secp256k1_zkp::Message::from_hashed_data::<sha256::Hash>(msg)
            .as_ref()
            .to_vec(),
    );

    Bip340Hash::hash(&buf).into_inner().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::thread_rng;

    fn verify(sig: &schnorrsig::Signature, msg: &[u8], pk: &schnorrsig::PublicKey) -> bool {
        let msg = secp256k1_zkp::Message::from_hashed_data::<sha256::Hash>(msg);
        SECP256K1.schnorrsig_verify(sig, &msg, pk).is_ok()
    }

    #[test]
    fn attest_and_verify() {
        let mut rng = thread_rng();

        let key_pair = schnorrsig::KeyPair::new(SECP256K1, &mut rng);
        let pk = schnorrsig::PublicKey::from_keypair(SECP256K1, &key_pair);

        let (nonce, _nonce_pk) = nonce(&mut rng);

        let msg = b"hello world";

        let sig = attest(&key_pair, &nonce, msg);

        assert!(verify(&sig, msg, &pk));
    }
}
