use crate::deprecated;
use crate::deprecated::protocol::*;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use asynchronous_codec::Framed;
use asynchronous_codec::JsonCodec;
use bdk::bitcoin::Txid;
use bdk_ext::keypair;
use futures::SinkExt;
use futures::StreamExt;
use maia_core::secp256k1_zkp::SecretKey;
use maia_core::secp256k1_zkp::XOnlyPublicKey;
use model::libp2p::PeerId;
use model::olivia::BitMexPriceEventId;
use model::Dlc;
use model::ExecuteOnCfd;
use model::OrderId;
use model::Role;
use model::Timestamp;
use std::time::Duration;
use tokio_extras::FutureExt;
use xtra::Address;
use xtra_libp2p::Endpoint;
use xtra_libp2p::OpenSubstream;
use xtra_libp2p::Substream;
use xtra_productivity::xtra_productivity;

/// The duration that the taker waits until a decision (accept/reject) is expected from the maker
///
/// If the maker does not respond within `DECISION_TIMEOUT` seconds then the taker will fail the
/// rollover.
const DECISION_TIMEOUT: Duration = Duration::from_secs(30);

/// One actor to rule all the rollovers
pub struct Actor<E, O> {
    endpoint: Address<Endpoint>,
    oracle_pk: XOnlyPublicKey,
    oracle: O,
    n_payouts: usize,
    executor: E,
}

#[async_trait]
impl<E, O> xtra::Actor for Actor<E, O>
where
    E: Send + Sync + 'static,
    O: Send + Sync + 'static,
{
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

#[derive(Copy, Clone)]
pub struct ProposeRollover {
    pub order_id: OrderId,
    pub maker_peer_id: PeerId,
    pub from_commit_txid: Txid,
    pub from_settlement_event_id: BitMexPriceEventId,
}

impl<E, O> Actor<E, O> {
    pub fn new(
        endpoint: Address<Endpoint>,
        executor: E,
        oracle_pk: XOnlyPublicKey,
        get_announcement: O,
        n_payouts: usize,
    ) -> Self {
        Self {
            endpoint,
            executor,
            oracle: get_announcement,
            oracle_pk,
            n_payouts,
        }
    }
}

impl<E, O> Actor<E, O> {
    async fn open_substream(&self, peer_id: PeerId) -> Result<Substream> {
        let substream = self
            .endpoint
            .send(OpenSubstream::single_protocol(
                peer_id.inner(),
                deprecated::PROTOCOL,
            ))
            .await
            .context("Endpoint is disconnected")?
            .context("No connection to peer")?
            .await
            .context("Failed to open substream")?;

        Ok(substream)
    }
}

#[xtra_productivity]
impl<E, O> Actor<E, O>
where
    E: ExecuteOnCfd + Clone + Send + Sync + 'static,
    O: GetAnnouncements + Clone + Send + Sync + 'static,
{
    pub async fn handle(&mut self, msg: ProposeRollover, ctx: &mut xtra::Context<Self>) {
        let ProposeRollover {
            order_id,
            maker_peer_id,
            from_commit_txid,
            from_settlement_event_id,
        } = msg;

        let substream = match self
            .open_substream(maker_peer_id)
            .await
            .context("Failed to start rollover")
        {
            Ok(substream) => substream,
            Err(e) => {
                emit_failed(order_id, e, &self.executor).await;
                return;
            }
        };

        tokio_extras::spawn_fallible(
            &ctx.address().expect("self to be alive"),
            {
                let executor = self.executor.clone();
                let oracle = self.oracle.clone();
                let oracle_pk = self.oracle_pk;
                let n_payouts = self.n_payouts;
                async move {
                    let mut framed = asynchronous_codec::Framed::new(
                        substream,
                        asynchronous_codec::JsonCodec::<DialerMessage, ListenerMessage>::new(),
                    );

                    let contract_symbol = executor
                        .execute(order_id, |cfd| {
                            let event = cfd.start_rollover_taker()?;
                            let contract_symbol = cfd.contract_symbol();

                            Ok((event, contract_symbol))
                        })
                        .await?;

                    framed
                        .send(DialerMessage::Propose(Propose {
                            order_id,
                            timestamp: Timestamp::now(),
                            from_commit_txid,
                        }))
                        .await
                        .context("Failed to send Msg0")?;

                    match framed
                        .next()
                        .timeout(DECISION_TIMEOUT, || {
                            tracing::debug_span!("receive decision")
                        })
                        .await
                        .with_context(|| {
                            format!(
                                "Maker did not accept/reject within {} seconds.",
                                DECISION_TIMEOUT.as_secs()
                            )
                        })?
                        .context("End of stream while receiving rollover decision from maker")?
                        .context("Failed to decode rollover decision from maker")?
                        .into_decision()?
                    {
                        Decision::Confirm(Confirm {
                            order_id,
                            oracle_event_ids,
                            tx_fee_rate,
                            funding_rate,
                            complete_fee,
                        }) => {
                            let (rollover_params, dlc, position) = executor
                                .execute(order_id, |cfd| {
                                    cfd.handle_rollover_accepted_taker(
                                        tx_fee_rate,
                                        funding_rate,
                                        &oracle_event_ids,
                                        from_settlement_event_id,
                                    )
                                })
                                .await?;

                            let announcements = oracle
                                .get_announcements(oracle_event_ids)
                                .await
                                .context("Failed to get announcement")?;
                            let settlement_event_id =
                                announcements.last().context("Empty to_event_ids")?.id;

                            tracing::info!(%order_id, "Rollover proposal got accepted");

                            let funding_fee = *rollover_params.funding_fee();
                            let complete_fee_before_rollover =
                                rollover_params.complete_fee_before_rollover();
                            let our_role = Role::Taker;
                            let our_position = position;

                            let (rev_sk, rev_pk) = keypair::new(&mut rand::thread_rng());
                            let (publish_sk, publish_pk) = keypair::new(&mut rand::thread_rng());

                            framed
                                .send(DialerMessage::RolloverMsg(Box::new(RolloverMsg::Msg0(
                                    RolloverMsg0 {
                                        revocation_pk: rev_pk,
                                        publish_pk,
                                    },
                                ))))
                                .await
                                .context("Failed to send Msg0")?;

                            fn next_rollover_span() -> tracing::Span {
                                tracing::debug_span!("next rollover message")
                            }

                            let msg0 = framed
                                .next()
                                .timeout(ROLLOVER_MSG_TIMEOUT, next_rollover_span)
                                .await
                                .with_context(|| {
                                    format!(
                                        "Expected Msg0 within {} seconds",
                                        ROLLOVER_MSG_TIMEOUT.as_secs()
                                    )
                                })?
                                .context("Empty stream instead of Msg0")?
                                .context("Unable to decode listener Msg0")?
                                .into_rollover_msg()?
                                .try_into_msg0()?;

                            let punish_params = PunishParams::new(
                                msg0.revocation_pk,
                                rev_pk,
                                msg0.publish_pk,
                                publish_pk,
                            );

                            let own_cfd_txs = build_own_cfd_transactions(
                                &dlc,
                                rollover_params,
                                announcements.clone(),
                                oracle_pk,
                                our_position,
                                n_payouts,
                                complete_fee.into(),
                                punish_params,
                                Role::Taker,
                                contract_symbol,
                            )
                            .await?;

                            framed
                                .send(DialerMessage::RolloverMsg(Box::new(RolloverMsg::Msg1(
                                    RolloverMsg1::from(own_cfd_txs.clone()),
                                ))))
                                .await
                                .context("Failed to send Msg1")?;

                            let msg1 = framed
                                .next()
                                .timeout(ROLLOVER_MSG_TIMEOUT, next_rollover_span)
                                .await
                                .with_context(|| {
                                    format!(
                                        "Expected Msg1 within {} seconds",
                                        ROLLOVER_MSG_TIMEOUT.as_secs()
                                    )
                                })?
                                .context("Empty stream instead of Msg1")?
                                .context("Unable to decode listener Msg1")?
                                .into_rollover_msg()?
                                .try_into_msg1()?;

                            let commit_desc = build_commit_descriptor(
                                dlc.identity_counterparty,
                                dlc.identity_pk(),
                                punish_params,
                            );
                            let (cets, refund_tx) = build_and_verify_cets_and_refund(
                                &dlc,
                                oracle_pk,
                                publish_pk,
                                our_role,
                                &own_cfd_txs,
                                &commit_desc,
                                &msg1,
                            )
                            .await?;

                            // reveal revocation secrets to the counterparty
                            framed
                                .send(DialerMessage::RolloverMsg(Box::new(RolloverMsg::Msg2(
                                    RolloverMsg2 {
                                        revocation_sk: dlc.revocation,
                                    },
                                ))))
                                .await
                                .context("Failed to send Msg2")?;

                            let base_dlc_params =
                                dlc.base_dlc_params_from_latest(complete_fee_before_rollover);

                            let revocation_sk_theirs =
                                match receive_maker_revocation_key(&mut framed).await {
                                    Ok(revocation_sk_theirs) => Some(revocation_sk_theirs),
                                    Err(e) => {
                                        tracing::warn!(
                                        "Finalising rollover without maker's revocation key: {e:#}"
                                    );
                                        None
                                    }
                                };

                            let revoked_commits = base_dlc_params
                                .revoke_base_commit_tx(revocation_sk_theirs)
                                .context("Maker sent invalid revocation sk")?;

                            let dlc = Dlc {
                                identity: dlc.identity,
                                identity_counterparty: dlc.identity_counterparty,
                                revocation: rev_sk,
                                revocation_pk_counterparty: punish_params.maker.revocation_pk,
                                publish: publish_sk,
                                publish_pk_counterparty: punish_params.maker.publish_pk,
                                maker_address: dlc.maker_address,
                                taker_address: dlc.taker_address,
                                lock: dlc.lock.clone(),
                                commit: (own_cfd_txs.commit.0.clone(), msg1.commit, commit_desc),
                                cets,
                                refund: (refund_tx, msg1.refund),
                                maker_lock_amount: dlc.maker_lock_amount,
                                taker_lock_amount: dlc.taker_lock_amount,
                                revoked_commit: revoked_commits,
                                settlement_event_id,
                                refund_timelock: rollover_params.refund_timelock,
                            };

                            emit_completed(
                                order_id,
                                dlc,
                                funding_fee,
                                complete_fee.into(),
                                &executor,
                            )
                            .await;
                        }
                        Decision::Reject(_) => {
                            emit_rejected(order_id, &executor).await;
                        }
                    }
                    Ok(())
                }
            },
            {
                let executor = self.executor.clone();
                move |e| async move {
                    emit_failed(order_id, e, &executor).await;
                }
            },
        );
    }
}

async fn receive_maker_revocation_key(
    framed: &mut Framed<Substream, JsonCodec<DialerMessage, ListenerMessage>>,
) -> Result<SecretKey> {
    let msg2 = framed
        .next()
        .timeout(ROLLOVER_MSG_TIMEOUT, || {
            tracing::debug_span!("next rollover message")
        })
        .await
        .with_context(|| {
            format!(
                "Expected Msg2 within {} seconds",
                ROLLOVER_MSG_TIMEOUT.as_secs()
            )
        })?
        .context("Empty stream instead of Msg2")?
        .context("Unable to decode listener Msg2")?
        .into_rollover_msg()?
        .try_into_msg2()?;

    Ok(msg2.revocation_sk)
}
