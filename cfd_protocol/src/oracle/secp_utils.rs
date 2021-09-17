use secp256k1_zkp::secp256k1_zkp_sys::types::c_void;
use secp256k1_zkp::secp256k1_zkp_sys::CPtr;
use secp256k1_zkp::{schnorrsig, SecretKey, SECP256K1};
use std::os::raw::{c_int, c_uchar};
use std::ptr;

/// Create a Schnorr signature using the provided nonce instead of generating one.
pub fn schnorr_sign_with_nonce(
    msg: &secp256k1_zkp::Message,
    keypair: &schnorrsig::KeyPair,
    nonce: &SecretKey,
) -> schnorrsig::Signature {
    unsafe {
        let mut sig = [0u8; secp256k1_zkp::constants::SCHNORRSIG_SIGNATURE_SIZE];
        assert_eq!(
            1,
            secp256k1_zkp::ffi::secp256k1_schnorrsig_sign(
                *SECP256K1.ctx(),
                sig.as_mut_c_ptr(),
                msg.as_c_ptr(),
                keypair.as_ptr(),
                Some(constant_nonce_fn),
                nonce.as_c_ptr() as *const c_void
            )
        );

        schnorrsig::Signature::from_slice(&sig).unwrap()
    }
}

extern "C" fn constant_nonce_fn(
    nonce32: *mut c_uchar,
    _msg32: *const c_uchar,
    _key32: *const c_uchar,
    _xonly_pk32: *const c_uchar,
    _algo16: *const c_uchar,
    data: *mut c_void,
) -> c_int {
    unsafe {
        ptr::copy_nonoverlapping(data as *const c_uchar, nonce32, 32);
    }
    1
}
