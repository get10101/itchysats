# ADR 011 - Process manager must only fail on saving the event

```
Author: @thomaseizinger
Date: 29/12/2021
Status: Active
```

## Summary

The `ProcessManager` MUST NOT fail for any reason other than saving the event to the database.

## Context

[ADR 010](./010-process-manager-for-orchestration.md) introduces the concept of a process manager for gluing together the creation of a domain event with further behaviour required to keep the system running.
For example, after a contract is set up, we need to broadcast the lock transaction, start monitoring etc.
All of these actions can potentially fail for various reasons.
See [here](https://github.com/itchysats/itchysats/blob/257078395fe24c52308ec740931283d4fba731ba/daemon/src/process_manager.rs#L65-L90) for an example.

The events in the database represent the source of truth in regard to the state of the system.
Once the event is saved (which implies checks like OCC passing, see [ADR 008](./008-occ-for-race-conditions.md)), all other components of the system will treat this event as final.

Failing the entire function _after_ the event is saved would signal to the caller that the entire processing of the event failed.
It would be unexpected to - in the case of a failure - still find said event in the database.

How we deal with these errors is out-of-scope of this decision.
