use bdk::bitcoin::hashes::Hash;
use bdk::bitcoin::SigHash;

pub(crate) trait SigHashExt {
    fn to_message(self) -> secp256k1_zkp::Message;
}

impl SigHashExt for SigHash {
    fn to_message(self) -> secp256k1_zkp::Message {
        use secp256k1_zkp::bitcoin_hashes::Hash;
        let hash = secp256k1_zkp::bitcoin_hashes::sha256d::Hash::from_inner(*self.as_inner());

        hash.into()
    }
}
