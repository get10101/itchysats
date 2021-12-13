use daemon::model::BitMexPriceEventId;
use daemon::oracle::{self};
use maia::secp256k1_zkp::schnorrsig;
use maia::secp256k1_zkp::SecretKey;
use std::str::FromStr;

#[allow(dead_code)]
pub struct OliviaData {
    id: BitMexPriceEventId,
    pk: schnorrsig::PublicKey,
    nonce_pks: Vec<schnorrsig::PublicKey>,
    price: u64,
    attestations: Vec<SecretKey>,
}

impl OliviaData {
    // Note: protocol tests have example 1 as well, but so far we are not asserting on that level of
    // granularity; example 1 can be pulled in once needed
    pub fn example_0() -> Self {
        Self::example(
            Self::EVENT_ID_0,
            Self::PRICE_0,
            &Self::NONCE_PKS_0,
            &Self::ATTESTATIONS_0,
        )
    }

    /// Generate an example of all the data from `olivia` needed to test the
    /// CFD protocol end-to-end.
    fn example(id: &str, price: u64, nonce_pks: &[&str], attestations: &[&str]) -> Self {
        let oracle_pk = schnorrsig::PublicKey::from_str(Self::OLIVIA_PK).unwrap();

        let id = id.parse().unwrap();

        let nonce_pks = nonce_pks
            .iter()
            .map(|pk| schnorrsig::PublicKey::from_str(pk).unwrap())
            .collect();

        let attestations = attestations
            .iter()
            .map(|pk| SecretKey::from_str(pk).unwrap())
            .collect();

        Self {
            id,
            pk: oracle_pk,
            nonce_pks,
            attestations,
            price,
        }
    }

    pub fn announcement(&self) -> oracle::Announcement {
        oracle::Announcement {
            id: self.id,
            expected_outcome_time: self.id.timestamp(),
            nonce_pks: self.nonce_pks.clone(),
        }
    }

    const OLIVIA_PK: &'static str =
        "ddd4636845a90185991826be5a494cde9f4a6947b1727217afedc6292fa4caf7";

    const EVENT_ID_0: &'static str = "/x/BitMEX/BXBT/2021-10-05T02:00:00.price?n=20";
    const NONCE_PKS_0: [&'static str; 20] = [
        "d02d163cf9623f567c4e3faf851a9266ac1ede13da4ca4141f3a7717fba9a739",
        "bc310f26aa5addbc382f653d8530aaead7c25e3546abc24639f490e36d4bdb88",
        "2661375f570dcc32300d442e85b6d72dfa3232dccda45e8fb4a2d1e758d1d374",
        "fcc68fbf071d391b14c0867cb4defb5a8abc12418dff3dfc2f84fd4025cb2716",
        "cf5c2b7fe3851c64a7ff9635a9bfc50cdd301401d002f2da049f4c6a20e8457b",
        "14f1005d8c2832a2c4666dd732dd9bb3af9c8f70ebcdaec96869b1ca0c8e0de6",
        "299ee1c9c20fab8b067adf452a7d7661b5e7f5dd6bc707562805002e7cb8443e",
        "bcb4e5a594346de298993a7a31762f598b5224b977e23182369e9ed3e5127f78",
        "25e09a16ee5d469069abfb62cd5e1f20af50cf15241f571e64fa28b127304574",
        "3ed5a1422f43299caf281123aba88bd4bc61ec863f4afb79c7ce7663ad44db5d",
        "a7e0f61212735c192c4bf16e0a3e925e65f9f3feb6f1e5e8d6f5c18cf2dbb5a8",
        "a36a631015d9036d0c321fea7cf12f589aa196e7279b4a290de5112c2940e540",
        "b5bdd931f81970139e7301ac654b378077c3ed993ca7893ed93fee5fc6f7a782",
        "00090816e256b41e042dce38bde99ab3cf9482f9b066836988d3ed54833638e8",
        "3530408e93c251f5f488d3b1c608157177c459d6fab1966abebf765bcc9338d2",
        "603269ce88d112ff7fcfcaab82f228be97deca37f8190084d509c71b51a30432",
        "f0587414fcc6c56aef11d4a1d287ad6b55b237c5b8a5d5d93eb9ca06f6466ccf",
        "763009afb0ffd99c7b835488cb3b0302f3b78f59bbfd5292bedab8ef9da8c1b7",
        "3867af9048309a05004a164bdea09899f23ff1d83b6491b2b53a1b7b92e0eb2e",
        "688118e6b59e27944c277513db2711a520f4283c7c53a11f58d9f6a46d82c964",
    ];
    const PRICE_0: u64 = 49262;
    const ATTESTATIONS_0: [&'static str; 20] = [
        "5bc7663195971daaa1e3e6a81b4bca65882791644bc446fc060cbc118a3ace0f",
        "721d0cb56a0778a1ca7907f81a0787f34385b13f854c845c4c5539f7f6267958",
        "044aeef0d525c8ff48758c80939e95807bc640990cc03f53ab6fc0b262045221",
        "79f5175423ec6ee69c8d0e55251db85f3015c2edfa5a03095443fbbf35eb2282",
        "233b9ec549e9cc7c702109d29636db85a3ec63a66f3b53444bcc7586d36ca439",
        "2961a00320b7c9a70220060019a6ca88e18c205fadd2f873c174e5ccbbed527e",
        "bdb76e8f81c39ade4205ead9b68118757fc49ec22769605f26ef904b235283d6",
        "6e75dafedf4ed685513ec1f5c93508de4fad2be05b46001ac00c03474f4690e1",
        "cfcfc27eb9273b343b3042f0386e77efe329066be079788bb00ab47d72f26780",
        "2d931ffd2963e74566365674583abc427bdb6ae571c4887d81f1920f0850665d",
        "33b6f1112fa046cbc04be44c615e70519702662c1f72d8d49b3c4613614a8a46",
        "19e569b15410fa9a758c1a6c211eae8c1547efbe0ac6a7709902be93415f2f09",
        "d859dd5c9a58e1836d1eea3ebe7f48198a681d29e5a5cd6922532d2e94a53a1d",
        "3387eb2ad5e64cd102167766bb72b447f4a2e5129d161e422f9d41cd7d1cc281",
        "db35a9778a1e3abc8d8ab2f4a79346ae2154c9e0b4932d859d1f3e244f67ae76",
        "c3be969e8b889cfb2ece71123e6be5538a2d3a1229637b18bccc179073c38059",
        "6f73263f430e10b82d0fd06c4ddd3b8a6b58c3e756745bd0d9e71a399e517921",
        "0818c9c245d7d2162cd393c562a121f80405a27d22ae465e95030c31ebb4bd24",
        "b7c03f0bd6d63bd78ad4ea0f3452ff9717ba65ca42038e6e90a1aa558b7942dc",
        "90c4d8ec9f408ccb62a62daa993c20f2f86799e1fdea520c6d060418e55fd216",
    ];
}
