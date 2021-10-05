pub use secp256k1_zkp::*;

use anyhow::Result;
use secp256k1_zkp::schnorrsig;
use std::num::NonZeroU8;

/// Compute an attestation public key for the given oracle public key,
/// announcement nonce public key and outcome index.
pub fn attestation_pk(
    oracle_pk: &schnorrsig::PublicKey,
    nonce_pk: &schnorrsig::PublicKey,
    index: NonZeroU8,
) -> Result<secp256k1_zkp::PublicKey> {
    let nonce_pk = schnorr_pubkey_to_pubkey(nonce_pk);

    let mut nonce_pk_sum = nonce_pk;
    nonce_pk_sum.mul_assign(SECP256K1, &index_to_bytes(index))?;

    let oracle_pk = schnorr_pubkey_to_pubkey(oracle_pk);
    let attestation_pk = oracle_pk.combine(&nonce_pk_sum)?;

    Ok(attestation_pk)
}

fn schnorr_pubkey_to_pubkey(pk: &schnorrsig::PublicKey) -> secp256k1_zkp::PublicKey {
    let mut buf = Vec::<u8>::with_capacity(33);

    buf.push(0x02); // append even byte
    buf.extend(&pk.serialize());

    secp256k1_zkp::PublicKey::from_slice(&buf).expect("valid key")
}

fn index_to_bytes(index: NonZeroU8) -> [u8; 32] {
    let mut bytes = [0u8; 32];

    bytes[31] = index.get();

    bytes
}
