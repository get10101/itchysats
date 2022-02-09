use conquer_once::Lazy;
use maia::secp256k1_zkp::schnorrsig;
use time::format_description::FormatItem;
use time::macros::format_description;

pub const EVENT_TIME_FORMAT: &[FormatItem] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");

pub static PUBLIC_KEY: Lazy<schnorrsig::PublicKey> = Lazy::new(|| {
    "ddd4636845a90185991826be5a494cde9f4a6947b1727217afedc6292fa4caf7"
        .parse()
        .expect("static key to be valid")
});
