# ADR 005 - Actors for modelling protocols

```
Author: @thomaseizinger
Date: 29/12/2021
Status: Active
```

## Summary

Use actors to model entire protocols such as contract-setup.

## Context

One design goal of an actor system is isolation of failures.
One component within the system failing should not take down others.
A useful analogy here is to think of actors as light-weight processes or workers.
We can spawn an actor with a certain task, let it operate concurrently within the system and eventually have it come back to us with the result of the operation.

Applying this thinking to our domain means we can and should model the execution of all our protocols as _instances_ of an actor.
That is: one actor for executing `contract-setup`, one actor for executing `rollover` etc.

These actors should govern the entire execution of the protocol from start to finish.
Not only does this make the application more resilient, it also allows us to execute these protocols concurrently!
