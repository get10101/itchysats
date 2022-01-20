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
In a bull market, shorts pay longs.
In a bear market, longs pay shorts.

To harmonize, how these fees are paid and where they are coming from, we want to revise the rollover protocol as per the design below.

## The new protocol

### 1. We define fixed UTC timestamps for the funding periods.
   
The exact timestamps are not decided yet but it will be something along the lines of:
   - Every day at 0400 UTC.
   - Every day at 0800 UTC.
   - Every day at 1200 UTC.
   - Every day at 1600 UTC.
   - Every day at 2000 UTC.
   - Every day at 0000 UTC.

Running rollover at exactly these points makes implementations easier because they can assume that well-behaved peer will execute the protocol at these points in time.

It makes UI design easier because we can clearly state, when the next funding event happens and it does not depend on the maker that we are connected to.

### 2. Enforce agreement of the funding rate through the blockchain protocol

Maker to stream current funding rate to taker -> required for display in UI.
Taker use this funding rate to start rollover -> if they use the same, sig-verification succeeds.

### 3. Remove approval (accept / decline) from the protocol.
   
With (1) and (2) in mind, there is really no point in an external approval step for rollover.
As long as the CFD's is not committed on-chain, rollover will occur.
To opt out of rollover, either party can commit the CFD on-chain which will result in a payout at the end of the funding period.

### 4. Connection dialer sends initial trigger message

- Need unambiguous way to trigger protocol
- Scales to peer to peer setting

## Considerations

1. The new protocol requires being online at a very specific point in time.
   
   This requirement is already implied by the fact that we have an off-chain channel construct and we need to be constantly online to monitor for the broadcast of old commit transactions.
