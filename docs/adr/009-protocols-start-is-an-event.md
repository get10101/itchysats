# ADR 009 - Model "starting" a protocol as a domain event

```
Author: @thomaseizinger
Date: 29/12/2021
Status: Active
```

## Summary

Introduce `ContractSetupStarted`, `RolloverStarted` and `CollaborativeSettlementStarted` as domain events.

## Context

[ADR 007](./007-event-sourcing-for-cfds.md) introduced event sourcing for managing a CFD's state.
Currently, only the completion (with either failure or success) of a protocol like contract-setup is modelled as an event.
This avoids invalid start up states as the entire protocol state is in-memory until it completes.

Whilst attractive, this has numerous downsides:

1. We cannot easily detect that a certain protocol is already active and thus run the risk of executing it concurrently for the same CFD.
2. State changes to a CFD are only triggered upon domain events and the UI is only updated on state changes, thus not having such an event introduces additional complexity through manual updates via the projection actor.
3. We cannot easily detect that a _competing_ protocol is already active.
   For example, rollover and collaborative settlement are mutually exclusive and should not be executed at the same time.
   It is desirable to detect this scenario early.

The solution is to introduce three new events:

- `ContractSetupStarted`
- `RolloverStarted`
- `CollaborativeSettlementStarted`

Persisting these into the list of events on a CFD will make every rehydration aware of this new state and consequently, command invocations can react to this state and react accordingly by for example failing right away.
