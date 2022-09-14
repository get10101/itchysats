use crate::deprecated::protocol::*;
use anyhow::Context;
use async_trait::async_trait;
use asynchronous_codec::Framed;
use asynchronous_codec::JsonCodec;
use bdk_ext::keypair;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::PeerId;
use maia_core::secp256k1_zkp::XOnlyPublicKey;
use model::Dlc;
use model::ExecuteOnCfd;
use model::Position;
use model::Role;
use model::RolloverVersion;
use tokio_extras::FutureExt;
use xtra_libp2p::NewInboundSubstream;
use xtra_libp2p::Substream;
use xtra_productivity::xtra_productivity;

/// Permanent actor to handle incoming substreams for the `/itchysats/rollover/1.0.0`
/// protocol.
///
/// There is only one instance of this actor for all connections, meaning we must always spawn a
/// task whenever we interact with a substream to not block the execution of other connections.
pub struct Actor<E, O, R> {
    oracle_pk: XOnlyPublicKey,
    oracle: O,
    n_payouts: usize,
    executor: E,
    rates: R,
    is_accepting_rollovers: bool,
}

impl<E, O, R> Actor<E, O, R> {
    pub fn new(
        executor: E,
        oracle_pk: XOnlyPublicKey,
        oracle: O,
        rates: R,
        n_payouts: usize,
    ) -> Self {
        Self {
            oracle_pk,
            oracle,
            n_payouts,
            executor,
            rates,
            is_accepting_rollovers: true,
        }
    }
}

#[async_trait]
impl<E, O, R> xtra::Actor for Actor<E, O, R>
where
    E: Send + Sync + 'static,
    O: Send + Sync + 'static,
    R: Send + Sync + 'static,
{
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

#[xtra_productivity]
impl<E, O, R> Actor<E, O, R>
where
    E: ExecuteOnCfd + Clone + Send + Sync + 'static,
    O: GetAnnouncements + Clone + Send + Sync + 'static,
    R: GetRates + Clone + Send + Sync + 'static,
{
    async fn handle(&mut self, msg: UpdateConfiguration) {
        self.is_accepting_rollovers = msg.is_accepting_rollovers;
    }
}

#[xtra_productivity]
impl<E, O, R> Actor<E, O, R>
where
    E: ExecuteOnCfd + Clone + Send + Sync + 'static,
    O: GetAnnouncements + Clone + Send + Sync + 'static,
    R: GetRates + Clone + Send + Sync + 'static,
{
    async fn handle(&mut self, msg: NewInboundSubstream, ctx: &mut xtra::Context<Self>) {
        let NewInboundSubstream { peer_id, stream } = msg;
        let address = ctx.address().expect("we are alive");

        tokio_extras::spawn_fallible(
            &address.clone(),
            async move {
                let mut framed =
                    Framed::new(stream, JsonCodec::<ListenerMessage, DialerMessage>::new());

                let propose = framed
                    .next()
                    .await
                    .context("End of stream while receiving Propose")?
                    .context("Failed to decode Propose")?
                    .into_propose()?;

                address
                    .send(ProposeReceived {
                        propose,
                        framed,
                        peer_id,
                    })
                    .await?;

                anyhow::Ok(())
            },
            move |e| async move {
                tracing::warn!(%peer_id, "Failed to handle incoming rollover protocol: {e:#}")
            },
        );
    }

    async fn handle(&mut self, msg: ProposeReceived, ctx: &mut xtra::Context<Self>) {
        let ProposeReceived {
            propose,
            mut framed,
            peer_id,
        } = msg;
        let order_id = propose.order_id;

        let base_dlc_params = match self
            .executor
            .execute(order_id, |cfd| {
                cfd.verify_counterparty_peer_id(&peer_id.into())?;
                cfd.start_rollover_maker(propose.from_commit_txid)
            })
            .await
        {
            Ok(base_dlc_params) => base_dlc_params,
            Err(e) => {
                // We have to append failed to ensure that we can rollover in the future
                // The cfd logic might otherwise prevent us from starting a rollover if there is
                // still one ongoing that was not properly ended.
                emit_failed(order_id, e, &self.executor).await;

                return;
            }
        };

        let this = ctx.address().expect("we are alive");
        if !self.is_accepting_rollovers {
            emit_rejected(order_id, &self.executor).await;

            tokio_extras::spawn_fallible(
                &this,
                async move {
                    framed
                        .send(ListenerMessage::Decision(Decision::Reject(Reject {
                            order_id,
                        })))
                        .await
                },
                move |e| async move {
                    tracing::warn!(%order_id, "Failed to send reject rollover to the taker: {e:#}")
                },
            );

            return;
        }

        fn next_rollover_span() -> tracing::Span {
            tracing::debug_span!("next rollover message")
        }

        let task = {
            let executor = self.executor.clone();
            let oracle = self.oracle.clone();
            let rates = self.rates.clone();
            let oracle_pk = self.oracle_pk;
            let n_payouts = self.n_payouts;
            async move {
                let Rates {
                    funding_rate_long,
                    funding_rate_short,
                    tx_fee_rate,
                } = rates.get_rates().await.context("Failed to get rates")?;

                let (rollover_params, dlc, position, oracle_event_id, funding_rate) = executor
                    .execute(order_id, |cfd| {
                        let funding_rate = match cfd.position() {
                            Position::Long => funding_rate_long,
                            Position::Short => funding_rate_short,
                        };

                        let (event, params, dlc, position, oracle_event_id) = cfd
                            .accept_rollover_proposal_single_event(
                                tx_fee_rate,
                                funding_rate,
                                Some((
                                    base_dlc_params.settlement_event_id(),
                                    base_dlc_params.complete_fee(),
                                )),
                                RolloverVersion::V3,
                            )?;

                        Ok((event, params, dlc, position, oracle_event_id, funding_rate))
                    })
                    .await?;

                let complete_fee = rollover_params
                    .fee_account
                    .add_funding_fee(rollover_params.current_fee)
                    .settle();

                framed
                    .send(ListenerMessage::Decision(Decision::Confirm(Confirm {
                        order_id,
                        oracle_event_id,
                        tx_fee_rate,
                        funding_rate,
                        complete_fee: complete_fee.into(),
                    })))
                    .await
                    .context("Failed to send rollover confirmation message")?;

                let announcement = oracle
                    .get_announcements(vec![oracle_event_id])
                    .await
                    .context("Failed to get announcement")?;

                let funding_fee = *rollover_params.funding_fee();

                let our_role = Role::Maker;
                let our_position = position;

                let (rev_sk, rev_pk) = keypair::new(&mut rand::thread_rng());
                let (publish_sk, publish_pk) = keypair::new(&mut rand::thread_rng());

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
                    .context("Unable to decode dialer Msg0")?
                    .into_rollover_msg()?
                    .try_into_msg0()?;

                framed
                    .send(ListenerMessage::RolloverMsg(Box::new(RolloverMsg::Msg0(
                        RolloverMsg0 {
                            revocation_pk: rev_pk,
                            publish_pk,
                        },
                    ))))
                    .await
                    .context("Failed to send Msg0")?;

                let punish_params = build_punish_params(
                    our_role,
                    dlc.identity,
                    dlc.identity_counterparty,
                    msg0,
                    rev_pk,
                    publish_pk,
                );

                let own_cfd_txs = build_own_cfd_transactions(
                    &dlc,
                    rollover_params,
                    &announcement[0],
                    oracle_pk,
                    our_position,
                    n_payouts,
                    complete_fee,
                    punish_params,
                )
                .await?;

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
                    .context("Unable to decode dialer Msg1")?
                    .into_rollover_msg()?
                    .try_into_msg1()?;

                framed
                    .send(ListenerMessage::RolloverMsg(Box::new(RolloverMsg::Msg1(
                        RolloverMsg1::from(own_cfd_txs.clone()),
                    ))))
                    .await
                    .context("Failed to send Msg1")?;

                let commit_desc = build_commit_descriptor(punish_params);
                let (cets, refund_tx) = build_and_verify_cets_and_refund(
                    &dlc,
                    &announcement[0],
                    oracle_pk,
                    publish_pk,
                    our_role,
                    &own_cfd_txs,
                    &commit_desc,
                    &msg1,
                )
                .await?;

                let msg2 = framed
                    .next()
                    .timeout(ROLLOVER_MSG_TIMEOUT, next_rollover_span)
                    .await
                    .with_context(|| {
                        format!(
                            "Expected Msg2 within {} seconds",
                            ROLLOVER_MSG_TIMEOUT.as_secs()
                        )
                    })?
                    .context("Empty stream instead of Msg2")?
                    .context("Unable to decode dialer Msg2")?
                    .into_rollover_msg()?
                    .try_into_msg2()?;

                // reveal revocation secrets to the counterparty
                if let Err(e) = framed
                    .send(ListenerMessage::RolloverMsg(Box::new(RolloverMsg::Msg2(
                        RolloverMsg2 {
                            revocation_sk: base_dlc_params.revocation_sk_ours(),
                        },
                    ))))
                    .await
                {
                    // If the taker tries to rollover again, they will do so from a previous
                    // commit TXID compared to the maker's.
                    tracing::warn!(%order_id, "Failed to send revocation keys to taker: {e:#}");
                }

                let revocation_sk_theirs = msg2.revocation_sk;
                let revoked_commits = base_dlc_params
                    .revoke_base_commit_tx(revocation_sk_theirs)
                    .context("Taker sent invalid revocation sk")?;

                let dlc = Dlc {
                    identity: dlc.identity,
                    identity_counterparty: dlc.identity_counterparty,
                    revocation: rev_sk,
                    revocation_pk_counterparty: punish_params.counterparty_params().revocation_pk,
                    publish: publish_sk,
                    publish_pk_counterparty: punish_params.counterparty_params().publish_pk,
                    maker_address: dlc.maker_address,
                    taker_address: dlc.taker_address,
                    lock: dlc.lock.clone(),
                    commit: (own_cfd_txs.commit.0.clone(), msg1.commit, commit_desc),
                    cets,
                    refund: (refund_tx, msg1.refund),
                    maker_lock_amount: dlc.maker_lock_amount,
                    taker_lock_amount: dlc.taker_lock_amount,
                    revoked_commit: revoked_commits,
                    settlement_event_id: announcement[0].id,
                    refund_timelock: rollover_params.refund_timelock,
                };

                emit_completed(order_id, dlc, funding_fee, complete_fee, &executor).await;

                Ok(())
            }
        };

        let err_handler = {
            let executor = self.executor.clone();
            move |e| async move {
                emit_failed(order_id, e, &executor).await;
            }
        };

        tokio_extras::spawn_fallible(&this, task, err_handler);
    }
}

#[derive(Clone, Copy)]
pub struct UpdateConfiguration {
    is_accepting_rollovers: bool,
}

impl UpdateConfiguration {
    pub fn new(is_accepting_rollovers: bool) -> Self {
        Self {
            is_accepting_rollovers,
        }
    }
}

struct ProposeReceived {
    propose: Propose,
    framed: Framed<Substream, JsonCodec<ListenerMessage, DialerMessage>>,
    peer_id: PeerId,
}
