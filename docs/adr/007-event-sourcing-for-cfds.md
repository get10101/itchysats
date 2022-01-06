# ADR 007 - Use event sourcing for managing a CFD's state

```
Author: @thomaseizinger
Date: 29/12/2021
Status: Active
```

## Summary

Instead of recording the current state of the CFD and appending to that table, record the events that trigger the state transition and rebuild the current state on demand.

## Context

Currently, we are always saving the _current_ state of a CFD in the database by appending to a `states` table.
Not only does this store duplicated data, it is also a cumbersome programming model because data from earlier states needs to be passed through others if it needs to be accessed later.

The solution here is to use event sourcing instead.
Event sourcing means that we never persist the current state of the CFD.
Instead, only the events are saved and the current state is rebuilt from the events whenever needed.

Whilst many elements of the domain come up as "events", our implementation of event sourcing ONLY considers those that change the state of a CFD.
It does not apply to:

- The state of the connection to other peers
- Blockchain events (transactions confirming, timelocks expiring, etc)
- Oracle attestations
- Application lifecycle (started, stopped, etc)

Some of these events however **trigger** state changes to our system, yet they don't **represent** a CFD's state change.
This is a subtle, yet very important difference.
For example, the oracle attesting a price will trigger an attempt to decrypt a CET.
If successful, the resulting CET will be recorded in an event.
The CFD's state changed because it now has a decrypted CET available which is a result of the oracle attesting but the attestation itself is not a state change (and hence also not a domain event).
