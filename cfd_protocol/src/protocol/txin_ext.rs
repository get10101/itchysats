use bdk::bitcoin::secp256k1::Signature;
use bdk::bitcoin::TxIn;

pub(crate) trait TxInExt {
    fn find_map_signature<F, R>(&self, f: F) -> Option<R>
    where
        F: Fn(Signature) -> Option<R>;
}

impl TxInExt for TxIn {
    fn find_map_signature<F, R>(&self, f: F) -> Option<R>
    where
        F: Fn(Signature) -> Option<R>,
    {
        self.witness
            .iter()
            .filter_map(|elem| {
                let elem = elem.as_slice();
                Signature::from_der(&elem[..elem.len() - 1]).ok()
            })
            .find_map(f)
    }
}
