#[cfg(test)]
mod tests {
    use anyhow::Result;
    use bdk::{
        bitcoin::{
            hashes::hex::ToHex,
            secp256k1::{All, Secp256k1, SecretKey},
            util::{bip32::ExtendedPrivKey, psbt::PartiallySignedTransaction},
            Address, Amount, Network, OutPoint, PrivateKey, PublicKey, Script, Transaction, TxIn,
            TxOut,
        },
        descriptor::Descriptor,
        miniscript::{descriptor::Wsh, DescriptorTrait},
        wallet::AddressIndex,
    };
    use rand::{CryptoRng, RngCore, SeedableRng};
    use rand_chacha::ChaChaRng;

    /// Refund transaction fee. It is paid evenly by the maker and the
    /// taker.
    ///
    /// Ideally we don't commit to a transaction fee ahead of time and
    /// instead resort to fee-bumping. But that would be unfair for the
    /// party that executes the fee-bumping.
    ///
    /// TODO: Calculate reasonable fee given the fact that the
    /// transaction consists of 1 input and 2 outputs.
    const REFUND_TX_FEE: u64 = 10_000;

    #[test]
    fn run_cfd_protocol() {
        let mut rng = ChaChaRng::seed_from_u64(0);
        let secp = Secp256k1::new();

        let maker_dlc_amount = Amount::ONE_BTC;
        let taker_dlc_amount = Amount::ONE_BTC;

        let (_oracle_sk, oracle_pk) = make_keypair(&mut rng, &secp);
        let (maker_sk, maker_pk) = make_keypair(&mut rng, &secp);
        let (taker_sk, taker_pk) = make_keypair(&mut rng, &secp);

        let payouts = vec![
            Payout {
                message: Message::Win,
                maker_amount: Amount::from_btc(2.0).unwrap(),
                taker_amount: Amount::ZERO,
            },
            Payout {
                message: Message::Lose,
                maker_amount: Amount::ZERO,
                taker_amount: Amount::from_btc(2.0).unwrap(),
            },
        ];

        // FIXME: Choose a value based on the network time or current
        // block height
        let refund_timelock = 0;

        let maker_wallet = build_wallet(Amount::from_btc(0.4).unwrap(), 5).unwrap();
        let taker_wallet = build_wallet(Amount::from_btc(0.4).unwrap(), 5).unwrap();

        let maker_address = maker_wallet.get_address(AddressIndex::New).unwrap();
        let taker_address = taker_wallet.get_address(AddressIndex::New).unwrap();

        // NOTE: We are probably paying too many transaction fees
        let (maker_psbt, _) = {
            let mut builder = maker_wallet.build_tx();
            builder
                .ordering(bdk::wallet::tx_builder::TxOrdering::Bip69Lexicographic)
                .add_recipient(Script::new(), maker_dlc_amount.as_sat());
            builder.finish().unwrap()
        };

        let (taker_psbt, _) = {
            let mut builder = taker_wallet.build_tx();
            builder
                .ordering(bdk::wallet::tx_builder::TxOrdering::Bip69Lexicographic)
                .add_recipient(Script::new(), taker_dlc_amount.as_sat());
            builder.finish().unwrap()
        };

        let (lock_psbt, dlc_outpoint) = create_lock_psbt(
            maker_psbt,
            taker_psbt,
            maker_pk,
            taker_pk,
            maker_dlc_amount + taker_dlc_amount,
        )
        .unwrap();

        let refund_tx = create_refund_tx(
            dlc_outpoint,
            refund_timelock,
            &maker_address,
            &taker_address,
            maker_dlc_amount,
            taker_dlc_amount,
        );

        let cets = payouts
            .iter()
            .map(|payout| create_cet(dlc_outpoint, payout, &maker_address, &taker_address))
            .collect::<Vec<_>>();

        dbg!(lock_psbt);
        dbg!(refund_tx);
        dbg!(cets);

        // TODO: Exchange signatures on refund transaction
        // TODO: Exchange encsignatures on all CETs
        // TODO: Exchange signatures on lock transaction
        // TODO: Verify validity of all tranasactions using bitcoinconsensus via bdk
    }

    #[derive(Debug, Clone)]
    struct ContractExecutionTransaction {
        inner: Transaction,
        message: Message,
    }

    fn create_cet(
        dlc_outpoint: OutPoint,
        payout: &Payout,
        maker_address: &Address,
        taker_address: &Address,
    ) -> ContractExecutionTransaction {
        let dlc_input = TxIn {
            previous_output: dlc_outpoint,
            ..Default::default()
        };

        let inner = Transaction {
            version: 2,
            lock_time: 0,
            input: vec![dlc_input],
            output: payout.to_txouts(maker_address, taker_address),
        };

        ContractExecutionTransaction {
            inner,
            message: payout.message,
        }
    }

    fn create_refund_tx(
        dlc_outpoint: OutPoint,
        lock_time: u32,
        maker_address: &Address,
        taker_address: &Address,
        maker_amount: Amount,
        taker_amount: Amount,
    ) -> Transaction {
        let dlc_input = TxIn {
            previous_output: dlc_outpoint,
            ..Default::default()
        };

        let per_party_fee = REFUND_TX_FEE / 2;

        let maker_output = TxOut {
            value: maker_amount.as_sat() - per_party_fee,
            script_pubkey: maker_address.script_pubkey(),
        };

        let taker_output = TxOut {
            value: taker_amount.as_sat() - per_party_fee,
            script_pubkey: taker_address.script_pubkey(),
        };

        Transaction {
            version: 2,
            lock_time,
            input: vec![dlc_input],
            output: vec![maker_output, taker_output],
        }
    }

    fn dlc_outpoint(lock_tx: &Transaction, dlc_descriptor: &Descriptor<PublicKey>) -> OutPoint {
        let txid = lock_tx.txid();
        let vout = lock_tx
            .output
            .iter()
            .position(|out| out.script_pubkey == dlc_descriptor.script_pubkey())
            .expect("to find dlc output in lock tx");

        OutPoint {
            txid,
            vout: vout as u32,
        }
    }

    fn create_lock_psbt(
        maker_psbt: PartiallySignedTransaction,
        taker_psbt: PartiallySignedTransaction,
        maker_pk: PublicKey,
        taker_pk: PublicKey,
        dlc_amount: Amount,
    ) -> Result<(PartiallySignedTransaction, OutPoint)> {
        let dlc_descriptor = build_dlc_descriptor(maker_pk, taker_pk);

        let maker_change = maker_psbt
            .global
            .unsigned_tx
            .output
            .into_iter()
            .filter(|out| !out.script_pubkey.is_empty())
            .collect::<Vec<_>>();

        let taker_change = taker_psbt
            .global
            .unsigned_tx
            .output
            .into_iter()
            .filter(|out| !out.script_pubkey.is_empty())
            .collect();

        let dlc_output = TxOut {
            value: dlc_amount.as_sat(),
            script_pubkey: dlc_descriptor
                .address(Network::Regtest)
                .expect("can derive address from descriptor")
                .script_pubkey(),
        };

        let lock_tx = Transaction {
            version: 2,
            lock_time: 0,
            input: vec![
                maker_psbt.global.unsigned_tx.input,
                taker_psbt.global.unsigned_tx.input,
            ]
            .concat(),
            output: vec![vec![dlc_output], maker_change, taker_change].concat(),
        };

        let dlc_outpoint = dlc_outpoint(&lock_tx, &dlc_descriptor);
        let lock_psbt = PartiallySignedTransaction::from_unsigned_tx(lock_tx)?;

        Ok((lock_psbt, dlc_outpoint))
    }

    fn build_wallet(
        utxo_amount: Amount,
        num_utxos: u8,
    ) -> Result<bdk::Wallet<(), bdk::database::MemoryDatabase>> {
        use bdk::populate_test_db;
        use bdk::testutils;

        // FIXME: Using the same key for every instance of the wallet will lead to bugs in the tests
        let key = "tprv8ZgxMBicQKsPeZRHk4rTG6orPS2CRNFX3njhUXx5vj9qGog5ZMH4uGReDWN5kCkY3jmWEtWause41CDvBRXD1shKknAMKxT99o9qUTRVC6m".parse::<ExtendedPrivKey>().unwrap();

        let descriptors = testutils!(@descriptors (&format!("wpkh({}/*)", key)));

        let mut database = bdk::database::MemoryDatabase::new();

        for index in 0..num_utxos {
            populate_test_db!(
                &mut database,
                testutils! {
                    @tx ( (@external descriptors, index as u32) => utxo_amount.as_sat() ) (@confirmations 1)
                },
                Some(100)
            );
        }

        let wallet = bdk::Wallet::new_offline(&descriptors.0, None, Network::Regtest, database)?;

        Ok(wallet)
    }

    // NOTE: This is a simplification. Our use-case will not work with
    // a simple enumeration of possible messages
    #[derive(Debug, Clone, Copy)]
    struct Payout {
        message: Message,
        maker_amount: Amount,
        taker_amount: Amount,
    }

    impl Payout {
        fn to_txouts(self, maker_address: &Address, taker_address: &Address) -> Vec<TxOut> {
            [
                (self.maker_amount, maker_address),
                (self.taker_amount, taker_address),
            ]
            .iter()
            .filter_map(|(amount, address)| {
                (amount == &Amount::ZERO).then(|| TxOut {
                    value: amount.as_sat(),
                    script_pubkey: address.script_pubkey(),
                })
            })
            .collect()
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum Message {
        Win,
        Lose,
    }

    pub fn build_dlc_descriptor(maker_pk: PublicKey, taker_pk: PublicKey) -> Descriptor<PublicKey> {
        const MINISCRIPT_TEMPLATE: &str = "c:and_v(v:pk(A),pk_k(B))";

        // NOTE: This shouldn't be a source of error, but maybe it is
        let maker_pk = ToHex::to_hex(&maker_pk.key);
        let taker_pk = ToHex::to_hex(&taker_pk.key);

        let miniscript = MINISCRIPT_TEMPLATE
            .replace("A", &maker_pk)
            .replace("B", &taker_pk);

        let miniscript = miniscript.parse().expect("a valid miniscript");

        Descriptor::Wsh(Wsh::new(miniscript).expect("a valid descriptor"))
    }

    fn make_keypair<R>(rng: &mut R, secp: &Secp256k1<All>) -> (SecretKey, PublicKey)
    where
        R: RngCore + CryptoRng,
    {
        let sk = SecretKey::new(rng);
        let pk = PublicKey::from_private_key(
            secp,
            &PrivateKey {
                compressed: true,
                network: Network::Regtest,
                key: sk,
            },
        );

        (sk, pk)
    }
}
