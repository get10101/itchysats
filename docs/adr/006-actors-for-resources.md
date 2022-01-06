# ADR 006 - Actors for modelling resources

```
Author: @thomaseizinger
Date: 29/12/2021
Status: Active
```

## Summary

Use actors for modelling resources like connections to other systems.

## Context

Actors are a great pattern to increase the resilience of a system.
However, that only works if applied correctly.

An actor has a lifetime, similar to a variable within a scope.
It is created, can be used for operations and eventually destroyed / shut down.

Resources like connections to our systems follow a similar path.
They are established, can be used to send / receive messages and can be shut down again.
Modelling such resources as actors is therefore a natural fit.

Sending a message to an actor that is not alive will yield `Disconnected`.
If the actor's lifetime tracks the lifetime of the connection, the rest of the system can deal with this connection disappearing in a resilient manner by handling `Disconnected` accordingly.

Additionally, tracking the lifetime of a resource with an actor allows us to make use of the supervisor pattern to restart it in case of failures.
