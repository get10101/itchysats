# ADR 004 - Actors

```
Author: @thomaseizinger
Date: 29/12/2021
Status: Active
```

## Summary

Use an actor library to implement the functionality of itchysats.

## Context

Itchysats is expected to be a system with a high number of concurrent processes.
We need to monitor the status of multiple transactions on the blockchain, receive and send messages to other peers, update clients in real-time and interact with external systems such as oracles.
Failures in any of these sub-systems should not affect the availability of other systems.

Actors are a great fit for this kind of problem.
An actor represents a process that can be interacted with by sending messages to it, much like one can call a function on an object in OOP.
Contrary to objects though, actors are intrinsically concurrent: Multiple other actors can hold an address to a single actor and send messages to it.
These messages will be processed sequentially, which allows actors to mutate their state without incurring race conditions.
