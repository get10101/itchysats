use crate::bitcoin::secp256k1::SecretKey;
use crate::bitcoin::PublicKey;
use crate::order::deprecated::protocol::Msg0;
use crate::order::deprecated::protocol::Msg1;
use crate::order::deprecated::protocol::Msg2;
use crate::order::deprecated::protocol::Msg3;
use crate::wallet;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use bdk::bitcoin::secp256k1::ecdsa::Signature;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::Amount;
use bdk::bitcoin::Transaction;
use bdk::miniscript::Descriptor;
use bdk_ext::keypair;
use futures::Sink;
use futures::SinkExt;
use futures::Stream;
use futures::StreamExt;
use maia::commit_descriptor;
use maia::lock_descriptor;
use maia_core::secp256k1_zkp::EcdsaAdaptorSignature;
use maia_core::secp256k1_zkp::XOnlyPublicKey;
use maia_core::Cets;
use maia_core::CfdTransactions;
use maia_core::PartyParams;
use maia_core::PunishParams;
use model::olivia;
use model::olivia::BitMexPriceEventId;
use model::payout_curve::ETHUSD_MULTIPLIER;
use model::shared_protocol::verify_adaptor_signature;
use model::shared_protocol::verify_cets;
use model::shared_protocol::verify_signature;
use model::Cet;
use model::ContractSymbol;
use model::Dlc;
use model::OraclePayouts;
use model::Payouts;
use model::Position;
use model::Role;
use model::SetupParams;
use model::TransactionExt;
use model::CET_TIMELOCK;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::time::Duration;
use tokio_extras::FutureExt;
use tracing::instrument;
use tracing::Instrument;
use xtra::prelude::MessageChannel;

use super::protocol::SetupMsg;

/// How long contract setup protocol waits for the next message before giving up
///
/// 120s are currently needed to ensure that we can outlive times when the maker/taker are under
/// heavy message load. Failed contract setups are annoying compared to failed rollovers so we allow
/// more time to see them less often.
const CONTRACT_SETUP_MSG_TIMEOUT: Duration = Duration::from_secs(120);

/// Given an initial set of parameters, sets up the CFD contract with
/// the counterparty.
#[allow(clippy::too_many_arguments)]
#[instrument(
    name = "Setup contract"
    skip_all,
    err
)]
// TODO(restioson) semantic spans lining up with the logs here
pub async fn new(
    mut sink: impl Sink<SetupMsg, Error = anyhow::Error> + Unpin,
    mut stream: impl Stream<Item = SetupMsg> + Unpin,
    (oracle_pk, announcements): (XOnlyPublicKey, Vec<olivia::Announcement>),
    setup_params: SetupParams,
    build_party_params_channel: MessageChannel<wallet::BuildPartyParams, Result<PartyParams>>,
    sign_channel: MessageChannel<wallet::Sign, Result<PartiallySignedTransaction>>,
    own_role: Role,
    position: Position,
    n_payouts: usize,
) -> Result<Dlc> {
    tracing::debug!(?setup_params, ?own_role, ?position, ?n_payouts);
    tracing::trace!(?oracle_pk, ?announcements);

    let (own, own_punish, key_pairs) =
        own_setup_params(build_party_params_channel, setup_params).await?;

    sink.send(SetupMsg::Msg0(Msg0::from((own.clone(), own_punish))))
        .instrument(tracing::debug_span!("Send Msg0"))
        .await
        .context("Failed to send Msg0")?;
    let msg0 = stream
        .next()
        .timeout(CONTRACT_SETUP_MSG_TIMEOUT, stream_next_span)
        .await
        .with_context(|| {
            format!(
                "Expected Msg0 within {} seconds",
                CONTRACT_SETUP_MSG_TIMEOUT.as_secs()
            )
        })?
        .context("Empty stream instead of Msg0")?
        .try_into_msg0()?;

    let (counterparty, counterparty_punish) = msg0.into();

    let params = AllParams {
        own,
        own_punish,
        counterparty,
        counterparty_punish,
        own_role,
    };

    let (own_cfd_txs, settlement_event_id) = create_cfd_transactions(
        setup_params,
        &params,
        key_pairs,
        (oracle_pk, announcements),
        position,
        own_role,
        n_payouts,
    )
    .await?;

    sink.send(SetupMsg::Msg1(Msg1::from(own_cfd_txs.clone())))
        .instrument(tracing::debug_span!("Send Msg1"))
        .await
        .context("Failed to send Msg1")?;

    let msg1 = stream
        .next()
        .timeout(CONTRACT_SETUP_MSG_TIMEOUT, stream_next_span)
        .await
        .with_context(|| {
            format!(
                "Expected Msg1 within {} seconds",
                CONTRACT_SETUP_MSG_TIMEOUT.as_secs()
            )
        })?
        .context("Empty stream instead of Msg1")?
        .try_into_msg1()?;

    let verified = verify_all(
        &params,
        own_cfd_txs,
        oracle_pk,
        &msg1.commit,
        &msg1.refund,
        &msg1.cets,
    )
    .await?;

    let mut signed_lock_tx = sign_channel
        .send(wallet::Sign {
            psbt: verified.lock_tx,
        })
        .instrument(tracing::debug_span!("Send Sign to wallet actor"))
        .await
        .context("Failed to send message to wallet actor")?
        .context("Failed to sign transaction")?;

    sink.send(SetupMsg::Msg2(Msg2 {
        signed_lock: signed_lock_tx.clone(),
    }))
    .instrument(tracing::debug_span!("Send Msg2"))
    .await
    .context("Failed to send Msg2")?;

    let msg2 = stream
        .next()
        .timeout(CONTRACT_SETUP_MSG_TIMEOUT, stream_next_span)
        .await
        .with_context(|| {
            format!(
                "Expected Msg2 within {} seconds",
                CONTRACT_SETUP_MSG_TIMEOUT.as_secs()
            )
        })?
        .context("Empty stream instead of Msg2")?
        .try_into_msg2()?;

    tracing::debug_span!("Merge lock PSBTs").in_scope(|| {
        signed_lock_tx
            .combine(msg2.signed_lock)
            .context("Failed to merge lock PSBTs")
    })?;

    let cets = extract_counterparty_adaptor_sig(
        &params,
        verified.commit_tx.clone(),
        verified.commit_desc.clone(),
        verified.own_cets,
        msg1.cets,
    )
    .await?;

    // TODO: Remove send- and receiving ACK messages once we are able to handle incomplete DLC
    // monitoring
    sink.send(SetupMsg::Msg3(Msg3))
        .instrument(tracing::debug_span!("Send Msg3"))
        .await
        .context("Failed to send Msg3")?;
    let _ = stream
        .next()
        .timeout(CONTRACT_SETUP_MSG_TIMEOUT, stream_next_span)
        .await
        .with_context(|| {
            format!(
                "Expected Msg3 within {} seconds",
                CONTRACT_SETUP_MSG_TIMEOUT.as_secs()
            )
        })?
        .context("Empty stream instead of Msg3")?
        .try_into_msg3()?;

    Ok(Dlc {
        identity: key_pairs.identity.private,
        identity_counterparty: params.counterparty.identity_pk,
        revocation: key_pairs.revoke.private,
        revocation_pk_counterparty: params.counterparty_punish.revocation_pk,
        publish: key_pairs.publish.private,
        publish_pk_counterparty: params.counterparty_punish.publish_pk,
        maker_address: params.maker().address.clone(),
        taker_address: params.taker().address.clone(),
        lock: (signed_lock_tx.extract_tx(), verified.lock_desc),
        commit: (verified.commit_tx, msg1.commit, verified.commit_desc),
        cets,
        refund: (verified.refund_tx, msg1.refund),
        maker_lock_amount: params.maker().lock_amount,
        taker_lock_amount: params.taker().lock_amount,
        revoked_commit: Vec::new(),
        settlement_event_id,
        refund_timelock: setup_params.refund_timelock,
    })
}

fn stream_next_span() -> tracing::Span {
    tracing::debug_span!("Receive setup message")
}

#[derive(Copy, Clone)]
pub struct KeyPair {
    private: SecretKey,
    public: PublicKey,
}

impl From<(SecretKey, PublicKey)> for KeyPair {
    fn from((private, public): (SecretKey, PublicKey)) -> Self {
        KeyPair { private, public }
    }
}

#[derive(Copy, Clone)]
struct KeyPairs {
    identity: KeyPair,
    revoke: KeyPair,
    publish: KeyPair,
}

#[instrument(name = "Generate own params", skip_all, err)]
async fn own_setup_params(
    build_party_params_channel: MessageChannel<wallet::BuildPartyParams, Result<PartyParams>>,
    setup_params: SetupParams,
) -> Result<(PartyParams, PunishParams, KeyPairs)> {
    let key_pairs = KeyPairs {
        identity: keypair::new(&mut rand::thread_rng()).into(),
        revoke: keypair::new(&mut rand::thread_rng()).into(),
        publish: keypair::new(&mut rand::thread_rng()).into(),
    };

    let own = build_party_params_channel
        .send(wallet::BuildPartyParams {
            amount: setup_params.margin,
            identity_pk: key_pairs.identity.public,
            fee_rate: setup_params.tx_fee_rate,
        })
        .instrument(tracing::debug_span!(
            "Send BuildPartyParams to wallet actor"
        ))
        .await
        .context("Failed to send message to wallet actor")?
        .context("Failed to build party params")?;

    let own_punish = PunishParams {
        revocation_pk: key_pairs.revoke.public,
        publish_pk: key_pairs.publish.public,
    };

    Ok((own, own_punish, key_pairs))
}

#[instrument(name = "Create CFD transactions", skip_all, err)]
async fn create_cfd_transactions(
    setup_params: SetupParams,
    params: &AllParams,
    key_pairs: KeyPairs,
    (oracle_pk, announcements): (XOnlyPublicKey, Vec<olivia::Announcement>),
    position: Position,
    role: Role,
    n_payouts: usize,
) -> Result<(CfdTransactions, BitMexPriceEventId)> {
    let expected_margin = setup_params.counterparty_margin;
    let actual_margin = params.counterparty.lock_amount;

    if actual_margin != expected_margin {
        bail!(
            "Amounts sent by counterparty don't add up, expected margin {expected_margin} but got {actual_margin}"
        )
    }

    let settlement_event_id = announcements.last().context("Empty announcements")?.id;

    let payouts = match setup_params.contract_symbol {
        ContractSymbol::BtcUsd => Payouts::new_inverse_double_initial(
            (position, role),
            setup_params.price,
            setup_params.quantity,
            (setup_params.long_leverage, setup_params.short_leverage),
            n_payouts,
            setup_params.fee_account.settle(),
        )?,
        ContractSymbol::EthUsd => Payouts::new_quanto(
            (position, role),
            setup_params.price.to_u64(),
            setup_params.quantity.to_u64(),
            (setup_params.long_leverage, setup_params.short_leverage),
            n_payouts,
            ETHUSD_MULTIPLIER,
            setup_params.fee_account.settle(),
        )?,
    };
    let payouts_per_event = OraclePayouts::new(payouts, announcements)?;

    let own_cfd_txs = tokio::task::spawn_blocking({
        let maker_params = params.maker().clone();
        let taker_params = params.taker().clone();
        let maker_punish = *params.maker_punish();
        let taker_punish = *params.taker_punish();

        move || {
            maia::create_cfd_transactions(
                (maker_params, maker_punish),
                (taker_params, taker_punish),
                oracle_pk,
                (CET_TIMELOCK, setup_params.refund_timelock),
                payouts_per_event.into(),
                key_pairs.identity.private,
                setup_params.tx_fee_rate.to_u32(),
            )
        }
    })
    .await?
    .context("Failed to create CFD transactions")?;

    Ok((own_cfd_txs, settlement_event_id))
}

struct Verified {
    lock_tx: PartiallySignedTransaction,
    lock_desc: Descriptor<PublicKey>,
    commit_tx: Transaction,
    commit_desc: Descriptor<PublicKey>,
    refund_tx: Transaction,
    own_cets: Vec<Cets>,
}

#[instrument(name = "Verify all", skip_all, err)]
async fn verify_all(
    params: &AllParams,
    own_cfd_txs: CfdTransactions,
    oracle_pk: XOnlyPublicKey,
    commit_sig: &EcdsaAdaptorSignature,
    refund_sig: &Signature,
    counterparty_cets: &HashMap<String, Vec<(RangeInclusive<u64>, EcdsaAdaptorSignature)>>,
) -> Result<Verified> {
    let lock_desc = lock_descriptor(params.maker().identity_pk, params.taker().identity_pk);

    let lock_amount = params.maker().lock_amount + params.taker().lock_amount;

    let commit_desc = commit_descriptor(
        (
            params.maker().identity_pk,
            params.maker_punish().revocation_pk,
            params.maker_punish().publish_pk,
        ),
        (
            params.taker().identity_pk,
            params.taker_punish().revocation_pk,
            params.taker_punish().publish_pk,
        ),
    );

    let own_cets = own_cfd_txs.cets;
    let commit_tx = own_cfd_txs.commit.0.clone();

    let commit_amount = Amount::from_sat(commit_tx.output[0].value);

    verify_adaptor_signature(
        &commit_tx,
        &lock_desc,
        lock_amount,
        commit_sig,
        &params.own_punish.publish_pk,
        &params.counterparty.identity_pk,
    )
    .context("Commit adaptor signature does not verify")?;

    {
        let verify_own = tracing::debug_span!("Verify own cets");
        for own_grouped_cets in own_cets.clone() {
            let counterparty_cets = counterparty_cets
                .get(&own_grouped_cets.event.id)
                .cloned()
                .context("Expect event to exist in msg")?;

            tokio::task::spawn_blocking({
                let commit_desc = commit_desc.clone();
                let params_counterparty = params.counterparty.clone();
                let verify_own = verify_own.clone();
                move || {
                    let _g = verify_own.entered();
                    verify_cets(
                        (oracle_pk, own_grouped_cets.event.nonce_pks.clone()),
                        params_counterparty,
                        own_grouped_cets.cets,
                        counterparty_cets,
                        commit_desc,
                        commit_amount,
                    )
                    .context("CET signatures don't verify")?;

                    anyhow::Ok(())
                }
            })
            .instrument(verify_own.clone())
            .await??;
        }
    }

    let lock_tx = own_cfd_txs.lock;
    let refund_tx = own_cfd_txs.refund.0;

    verify_signature(
        &refund_tx,
        &commit_desc,
        commit_amount,
        refund_sig,
        &params.counterparty.identity_pk,
    )
    .context("Refund signature does not verify")?;

    Ok(Verified {
        lock_tx,
        lock_desc,
        commit_tx,
        commit_desc,
        refund_tx,
        own_cets,
    })
}

#[instrument(
    name = "Add counterparty adaptor signatures to our CETs",
    skip_all,
    err
)]
async fn extract_counterparty_adaptor_sig(
    params: &AllParams,
    commit_tx: Transaction,
    commit_desc: Descriptor<PublicKey>,
    own_cets: Vec<Cets>,
    counterparty_cets: HashMap<String, Vec<(RangeInclusive<u64>, EcdsaAdaptorSignature)>>,
) -> Result<HashMap<BitMexPriceEventId, Vec<Cet>>> {
    // TODO: In case we sign+send but never receive (the signed lock_tx from the counterparty)
    // we need some fallback handling (after x time) to spend the outputs in a different way so
    // the counterparty cannot hold us hostage
    let maker_address = params.maker().address.clone();
    let taker_address = params.taker().address.clone();

    tokio::task::spawn_blocking(move || {
        own_cets
                .into_iter()
                .map(|grouped_cets| {
                    let event_id = grouped_cets.event.id;
                    let counterparty_cets = counterparty_cets
                        .get(&event_id)
                        .with_context(|| format!("Counterparty CETs for event {event_id} missing"))?;
                    let cets = grouped_cets
                        .cets
                        .into_iter()
                        .map(|(tx, _, digits)| {
                            let counterparty_encsig = counterparty_cets
                                .iter()
                                .find_map(|(counterparty_range, counterparty_encsig)| {
                                    (counterparty_range == &digits.range()).then(|| counterparty_encsig)
                                })
                                .with_context(|| {
                                    let range = digits.range();

                                    format!(
                                        "Missing counterparty adaptor signature for CET corresponding to price range {range:?}",
                                    )
                                })?;

                            let maker_amount = tx.find_output_amount(&maker_address.script_pubkey()).unwrap_or_default();
                            let taker_amount = tx.find_output_amount(&taker_address.script_pubkey()).unwrap_or_default();

                            let cet = Cet {
                                maker_amount,
                                taker_amount,
                                adaptor_sig: *counterparty_encsig,
                                range: digits.range(),
                                n_bits: digits.len(),
                                txid: tx.txid(),
                            };

                            debug_assert_eq!(
                                cet.to_tx((&commit_tx, &commit_desc), &maker_address, &taker_address)
                                    .expect("can reconstruct CET")
                                    .txid(),
                                tx.txid()
                            );

                            Ok(cet)
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Ok((event_id.parse()?, cets))
                })
                .collect::<Result<HashMap<_, _>>>()
    }).await?
}

/// A convenience struct for storing PartyParams and PunishParams of both
/// parties and the role of the caller.
struct AllParams {
    pub own: PartyParams,
    pub own_punish: PunishParams,
    pub counterparty: PartyParams,
    pub counterparty_punish: PunishParams,
    pub own_role: Role,
}

impl AllParams {
    fn maker(&self) -> &PartyParams {
        match self.own_role {
            Role::Maker => &self.own,
            Role::Taker => &self.counterparty,
        }
    }

    fn taker(&self) -> &PartyParams {
        match self.own_role {
            Role::Maker => &self.counterparty,
            Role::Taker => &self.own,
        }
    }

    fn maker_punish(&self) -> &PunishParams {
        match self.own_role {
            Role::Maker => &self.own_punish,
            Role::Taker => &self.counterparty_punish,
        }
    }
    fn taker_punish(&self) -> &PunishParams {
        match self.own_role {
            Role::Maker => &self.counterparty_punish,
            Role::Taker => &self.own_punish,
        }
    }
}
