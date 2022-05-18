use std::collections::HashMap;
use crate::setup_contract;
use crate::wire::{CompleteFee, RolloverMsg0, RolloverMsg1, RolloverMsg2, RolloverMsg3};
use crate::wire::RolloverMsg;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use async_stream::stream;
use asynchronous_codec::{Framed, JsonCodec};
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::PeerId;
use model::olivia::BitMexPriceEventId;
use model::{calculate_payouts, Cet, CET_TIMELOCK, Dlc, FeeFlow, olivia, Position, RevokedCommit, Role, RolloverParams};
use model::FundingFee;
use model::FundingRate;
use model::OrderId;
use model::Timestamp;
use model::TxFeeRate;
use serde::Deserialize;
use serde::Serialize;
use xtra::Address;
use xtra_libp2p::{Endpoint, Substream};
use xtra_libp2p::OpenSubstream;
use bdk::bitcoin::secp256k1::{schnorrsig, SECP256K1};
use bdk::bitcoin::{Amount, PublicKey};
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::miniscript::DescriptorTrait;
use maia::{commit_descriptor, renew_cfd_transactions};
use maia_core::{Announcement, PartyParams, PunishParams, secp256k1_zkp};
use bdk_ext::keypair;
use crate::future_ext::FutureExt;
use crate::transaction_ext::TransactionExt;

use super::PROTOCOL;

pub struct Rollover {
    pub dlc: Dlc,
    pub funding_fee: FundingFee,
}

#[allow(clippy::too_many_arguments)]
pub async fn roll_over(
    mut framed: Framed<Substream, JsonCodec<DialerMessage, ListenerMessage>>,
    (oracle_pk, announcement): (schnorrsig::PublicKey, olivia::Announcement),
    rollover_params: RolloverParams,
    our_role: Role,
    our_position: Position,
    dlc: Dlc,
    n_payouts: usize,
    complete_fee: FeeFlow,
) -> Result<Dlc> {
    let sk = dlc.identity;
    let pk = PublicKey::new(secp256k1_zkp::PublicKey::from_secret_key(SECP256K1, &sk));

    let (rev_sk, rev_pk) = keypair::new(&mut rand::thread_rng());
    let (publish_sk, publish_pk) = keypair::new(&mut rand::thread_rng());

    let own_punish = PunishParams {
        revocation_pk: rev_pk,
        publish_pk,
    };

    framed
        .send(RolloverMsg::Msg0(RolloverMsg0 {
            revocation_pk: rev_pk,
            publish_pk,
        }))
        .await
        .context("Failed to send Msg0")?;
    let msg0 = framed
        .next()
        .timeout(ROLLOVER_MSG_TIMEOUT)
        .await
        .with_context(|| format_expect_msg_within("Msg0", ROLLOVER_MSG_TIMEOUT))?
        .try_into_msg0()?;

    let maker_lock_amount = dlc.maker_lock_amount;
    let taker_lock_amount = dlc.taker_lock_amount;
    let payouts = HashMap::from_iter([(
        Announcement {
            id: announcement.id.to_string(),
            nonce_pks: announcement.nonce_pks.clone(),
        },
        calculate_payouts(
            our_position,
            our_role,
            rollover_params.price,
            rollover_params.quantity,
            rollover_params.long_leverage,
            rollover_params.short_leverage,
            n_payouts,
            complete_fee,
        )?,
    )]);

    // unsign lock tx because PartiallySignedTransaction needs an unsigned tx
    let mut unsigned_lock_tx = dlc.lock.0.clone();
    unsigned_lock_tx
        .input
        .iter_mut()
        .for_each(|input| input.witness.clear());

    let lock_tx = PartiallySignedTransaction::from_unsigned_tx(unsigned_lock_tx)?;
    let other_punish_params = PunishParams {
        revocation_pk: msg0.revocation_pk,
        publish_pk: msg0.publish_pk,
    };
    let ((maker_identity, maker_punish_params), (taker_identity, taker_punish_params)) =
        match our_role {
            Role::Maker => (
                (pk, own_punish),
                (dlc.identity_counterparty, other_punish_params),
            ),
            Role::Taker => (
                (dlc.identity_counterparty, other_punish_params),
                (pk, own_punish),
            ),
        };
    let own_cfd_txs = tokio::task::spawn_blocking({
        let maker_address = dlc.maker_address.clone();
        let taker_address = dlc.taker_address.clone();
        let lock_tx = lock_tx.clone();

        move || {
            renew_cfd_transactions(
                lock_tx,
                (
                    maker_identity,
                    maker_lock_amount,
                    maker_address,
                    maker_punish_params,
                ),
                (
                    taker_identity,
                    taker_lock_amount,
                    taker_address,
                    taker_punish_params,
                ),
                oracle_pk,
                (CET_TIMELOCK, rollover_params.refund_timelock),
                payouts,
                sk,
                rollover_params.fee_rate.to_u32(),
            )
        }
    })
    .await?
    .context("Failed to create new CFD transactions")?;

    framed.send(RolloverMsg::Msg1(RolloverMsg1::from(own_cfd_txs.clone())))
        .await
        .context("Failed to send Msg1")?;

    let msg1 = framed
        .select_next_some()
        .timeout(ROLLOVER_MSG_TIMEOUT)
        .await
        .with_context(|| format_expect_msg_within("Msg1", ROLLOVER_MSG_TIMEOUT))?
        .try_into_msg1()?;

    let lock_amount = taker_lock_amount + maker_lock_amount;

    let commit_desc = commit_descriptor(
        (
            maker_identity,
            maker_punish_params.revocation_pk,
            maker_punish_params.publish_pk,
        ),
        (
            taker_identity,
            taker_punish_params.revocation_pk,
            taker_punish_params.publish_pk,
        ),
    );

    let own_cets = own_cfd_txs.cets;
    let commit_tx = own_cfd_txs.commit.0.clone();

    let commit_amount = Amount::from_sat(commit_tx.output[0].value);

    verify_adaptor_signature(
        &commit_tx,
        &dlc.lock.1,
        lock_amount,
        &msg1.commit,
        &publish_pk,
        &dlc.identity_counterparty,
    )
    .context("Commit adaptor signature does not verify")?;

    let other_address = match our_role {
        Role::Maker => dlc.taker_address.clone(),
        Role::Taker => dlc.maker_address.clone(),
    };

    for own_grouped_cets in own_cets.clone() {
        let other_cets = msg1
            .cets
            .get(&own_grouped_cets.event.id)
            .cloned()
            .context("Expect event to exist in msg")?;

        verify_cets(
            (oracle_pk, announcement.nonce_pks.clone()),
            PartyParams {
                lock_psbt: lock_tx.clone(),
                identity_pk: dlc.identity_counterparty,
                lock_amount,
                address: other_address.clone(),
            },
            own_grouped_cets.cets,
            other_cets,
            commit_desc.clone(),
            commit_amount,
        )
        .await
        .context("CET signatures don't verify")?;
    }

    let refund_tx = own_cfd_txs.refund.0;

    verify_signature(
        &refund_tx,
        &commit_desc,
        commit_amount,
        &msg1.refund,
        &dlc.identity_counterparty,
    )
    .context("Refund signature does not verify")?;

    let maker_address = &dlc.maker_address;
    let taker_address = &dlc.taker_address;
    let cets = own_cets
        .into_iter()
        .map(|grouped_cets| {
            let event_id = grouped_cets.event.id;
            let other_cets = msg1
                .cets
                .get(&event_id)
                .with_context(|| format!("Counterparty CETs for event {event_id} missing"))?;
            let cets = grouped_cets
                .cets
                .into_iter()
                .map(|(tx, _, digits)| {
                    let other_encsig = other_cets
                        .iter()
                        .find_map(|(other_range, other_encsig)| {
                            (other_range == &digits.range()).then(|| other_encsig)
                        })
                        .with_context(|| {
                            let range = digits.range();

                            format!(
                                "Missing counterparty adaptor signature for CET corresponding to
                                 price range {range:?}"
                            )
                        })?;

                    let maker_amount = tx
                        .find_output_amount(&maker_address.script_pubkey())
                        .unwrap_or_default();
                    let taker_amount = tx
                        .find_output_amount(&taker_address.script_pubkey())
                        .unwrap_or_default();
                    let cet = Cet {
                        maker_amount,
                        taker_amount,
                        adaptor_sig: *other_encsig,
                        range: digits.range(),
                        n_bits: digits.len(),
                        txid: tx.txid(),
                    };

                    debug_assert_eq!(
                        cet.to_tx((&commit_tx, &commit_desc), maker_address, taker_address)
                            .expect("can reconstruct CET")
                            .txid(),
                        tx.txid()
                    );

                    Ok(cet)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((event_id.parse()?, cets))
        })
        .collect::<Result<HashMap<_, _>>>()?;

    // reveal revocation secrets to the other party
    framed.send(RolloverMsg::Msg2(RolloverMsg2 {
        revocation_sk: dlc.revocation,
    }))
    .await
    .context("Failed to send Msg2")?;

    let msg2 = framed
        .select_next_some()
        .timeout(ROLLOVER_MSG_TIMEOUT)
        .await
        .with_context(|| format_expect_msg_within("Msg2", ROLLOVER_MSG_TIMEOUT))?
        .try_into_msg2()?;
    let revocation_sk_theirs = msg2.revocation_sk;

    {
        let derived_rev_pk = PublicKey::new(secp256k1_zkp::PublicKey::from_secret_key(
            SECP256K1,
            &revocation_sk_theirs,
        ));

        if derived_rev_pk != dlc.revocation_pk_counterparty {
            anyhow::bail!("Counterparty sent invalid revocation sk");
        }
    }

    let mut revoked_commit = dlc.revoked_commit;
    revoked_commit.push(RevokedCommit {
        encsig_ours: own_cfd_txs.commit.1,
        revocation_sk_theirs,
        publication_pk_theirs: dlc.publish_pk_counterparty,
        txid: dlc.commit.0.txid(),
        script_pubkey: dlc.commit.2.script_pubkey(),
    });

    // TODO: Remove send- and receiving ACK messages once we are able to handle incomplete DLC
    // monitoring
    framed.send(RolloverMsg::Msg3(RolloverMsg3))
        .await
        .context("Failed to send Msg3")?;
    let _ = framed
        .select_next_some()
        .timeout(ROLLOVER_MSG_TIMEOUT)
        .await
        .with_context(|| format_expect_msg_within("Msg3", ROLLOVER_MSG_TIMEOUT))?
        .try_into_msg3()?;

    Ok(Dlc {
        identity: sk,
        identity_counterparty: dlc.identity_counterparty,
        revocation: rev_sk,
        revocation_pk_counterparty: other_punish_params.revocation_pk,
        publish: publish_sk,
        publish_pk_counterparty: other_punish_params.publish_pk,
        maker_address: dlc.maker_address,
        taker_address: dlc.taker_address,
        lock: dlc.lock.clone(),
        commit: (commit_tx, msg1.commit, commit_desc),
        cets,
        refund: (refund_tx, msg1.refund),
        maker_lock_amount,
        taker_lock_amount,
        revoked_commit,
        settlement_event_id: announcement.id,
        refund_timelock: rollover_params.refund_timelock,
    })
}

// in other words, the taker
pub async fn dialer(
    endpoint: Address<Endpoint>,
    order_id: OrderId,
    counterparty: PeerId,
) -> Result<Rollover, DialerError> {
    let substream = endpoint
        .send(OpenSubstream::single_protocol(counterparty, PROTOCOL))
        .await
        .context("Endpoint is disconnected")?
        .context("Failed to open substream")?;
    let mut framed = asynchronous_codec::Framed::new(
        substream,
        asynchronous_codec::JsonCodec::<DialerMessage, ListenerMessage>::new(),
    );

    framed
        .send(DialerMessage::Propose(Propose {
            order_id,
            timestamp: Timestamp::now(),
        }))
        .await
        .context("Failed to send Msg0")?;

    // TODO: We will need to apply a timeout to these. Perhaps we can put a timeout generally into
    // "reading from the substream"?
    match framed
        .next()
        .await
        .context("End of stream while receiving Msg1")?
        .context("Failed to decode Msg1")?
        .into_decision()?
    {
        Decision::Confirm(_) => {
            // TODO: Add setup_contract::roll_over() invocation
            // setup_contract::roll_over(sink, stream, _, rollover_params, our_role, our_position, dlc, n_payouts, complete_fee)
        }
        Decision::Reject(_) => return Err(DialerError::Rejected),
    }
    todo!("Finish this function");
}

#[derive(thiserror::Error, Debug)]
pub enum DialerError {
    #[error("Rollover got rejected")]
    Rejected,
    #[error("Rollover failed")]
    Failed { source: anyhow::Error },
}

impl From<anyhow::Error> for DialerError {
    fn from(source: anyhow::Error) -> Self {
        Self::Failed { source }
    }
}

#[derive(Serialize, Deserialize)]
pub enum DialerMessage {
    Propose(Propose),
    RolloverMsg(RolloverMsg),
}

impl DialerMessage {
    pub fn into_propose(self) -> Result<Propose> {
        match self {
            DialerMessage::Propose(propose) => Ok(propose),
            DialerMessage::RolloverMsg(_) => Err(anyhow!("Expected Propose but got RolloverMsg")),
        }
    }

    pub fn into_rollover_msg(self) -> Result<RolloverMsg> {
        match self {
            DialerMessage::RolloverMsg(rollover_msg) => Ok(rollover_msg),
            DialerMessage::Propose(_) => Err(anyhow!("Expected RolloverMsg but got Propose")),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub enum Decision {
    Confirm(Confirm),
    Reject(Reject),
}

#[derive(Serialize, Deserialize)]
pub enum ListenerMessage {
    Decision(Decision),
    RolloverMsg(RolloverMsg),
}

impl ListenerMessage {
    pub fn into_decision(self) -> Result<Decision> {
        match self {
            ListenerMessage::Decision(decision) => Ok(decision),
            ListenerMessage::RolloverMsg(_) => {
                Err(anyhow!("Expected Decision but got RolloverMsg"))
            }
        }
    }

    pub fn into_rollover_msg(self) -> Result<RolloverMsg> {
        match self {
            ListenerMessage::RolloverMsg(rollover_msg) => Ok(rollover_msg),
            ListenerMessage::Decision(_) => Err(anyhow!("Expected RolloverMsg but got Decision")),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Propose {
    pub order_id: OrderId,
    pub timestamp: Timestamp,
}

#[derive(Serialize, Deserialize)]
pub struct Confirm {
    order_id: OrderId,
    oracle_event_id: BitMexPriceEventId,
    tx_fee_rate: TxFeeRate,
    funding_rate: FundingRate,
    complete_fee: CompleteFee,
}

#[derive(Serialize, Deserialize)]
pub struct Reject {
    pub order_id: OrderId,
}
