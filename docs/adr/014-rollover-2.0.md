# ADR 014 - Rollover 2.0

```
Author: @thomaseizinger
Date: 20/01/2022
Status: Active
```

## Summary

Revise the design of the rollover protocol to:

1. Run at fixed points in time.
2. Not require approval by either party.

## Context

Currently, rollover is a protocol that is initiated by the taker pretty much on the hour every hour.
The exact conditions for triggering are that we want the timestamp of the price event that is used for attestation to be 24h in the future.
Because the oracle attests on the price exactly every hour, we can rollover to a new price event on the hour.

Rollover is coupled to paying fees.
In particular, the time window until the attestation can be considered as the current funding period.
For every new funding period, fees are charged depending on the current market.
In a bull market, longs pay shorts.
In a bear market, shorts pay longs.

To harmonize, how these fees are paid and where they are coming from, we want to revise the rollover protocol as per the design below.

## The new protocol

### 1. We define fixed UTC timestamps for the funding periods
   
The exact timestamps are not decided yet, but they will be something along the lines of:
   - Every day at 0400 UTC.
   - Every day at 0800 UTC.
   - Every day at 1200 UTC.
   - Every day at 1600 UTC.
   - Every day at 2000 UTC.
   - Every day at 0000 UTC.

Running rollover at exactly these points makes implementations easier because they can assume that well-behaved peer will execute the protocol at these points in time.

It makes UI design easier because we can clearly state, when the next funding event happens, and it does not depend on the maker that we are connected to.

### 2. Each party should start the protocol with an expectation of the funding rate

In the future, the funding rate will be determined by the market.
As such, no party should be specifying the funding rate as part of the protocol.

### 3. Maker streams current funding rate to taker

To simulate a market whilst we don't yet have one, introduce a stream of funding rates from the maker to the taker.
This stream can be used to display the current funding rate in the UI.
Once we have a market, this component will be removed.

### 4. Enforce agreement of the funding rate through the blockchain protocol

With both parties entering the next funding period with a specific funding rate, the protocol will simply fail due to signature verification errors if the funding rates don't match.
This should be sufficient for an MVP of this new protocol.
More elaborate detection of what the cause of the failure was can easily be added on top.

### 5. Remove approval (accept / decline) from the protocol
   
With (1) and (2) in mind, there is really no point in an external approval step for rollover.
As long as the CFD's is not committed on-chain, rollover will occur.
To opt out of rollover, either party can commit the CFD on-chain which will result in a payout at the end of the funding period.

### 6. Connection dialer sends initial trigger message

- Need unambiguous way to trigger protocol
- Scales to peer to peer setting

## Considerations

1. The new protocol requires being online at a very specific point in time.
   
   This requirement is already implied by the fact that we have an off-chain channel construct and we need to be constantly online to monitor for the broadcast of old commit transactions.

## Open questions

1. Should we avoid the in-efficiency in doing a rollover when both parties don't align on the funding rate?
2. When should peers trigger rollover? 5min before the funding period ends? 10min? What is enough time?
3. With fixed funding periods, we get a lot of load on very specific points in time. Is that an issue? 
4. Should the stream of funding rates be incorporated into the current offer?
