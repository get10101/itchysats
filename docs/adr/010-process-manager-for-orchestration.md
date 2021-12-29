# ADR 010 - Process manager for orchestration

```
Author: @thomaseizinger
Date: 29/12/2021
Status: Active
```

## Summary

Use a [`ProcessManager`](https://blog.scooletz.com/2014/11/21/process-manager-in-event-sourcing/) to orchestrate the program's behaviour upon domain events.

## Context

In event sourcing, emitting events from commands is how we change the system's state.
However, saving the event to the database does not magically trigger other actions within the system.
For example, after we have set up a contract and emitted the `ContractSetupCompleted` event, we need to:

1. Broadcast the lock transaction
2. Start monitoring for blockchain events related to this contract
3. Start monitoring for the attestation the DLC is tied to

All of these actions are responsibilities of different actors.
They also incur IO and require knowledge of the actor system, two things that we do not want the model to be concerned with.

The solution to this problem is to introduce a `ProcessManager`.
To keep the solution simple, the `ProcessManager` should only be concerned about the mapping of `Event` -> `Action`.
Therefore, the `ProcessManager` should use `MessageChannel`s instead of `Address`es to avoid unnecessary coupling to specific actors.

The `ProcessManager` **is not** a general-purpose orchestration component.
It is only intended to process CFD domain event.
