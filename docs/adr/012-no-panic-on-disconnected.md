# ADR 012 - Do not panic on actors being `Disconnected`

```
Author: @thomaseizinger
Date: 29/12/2021
Status: Active
```

## Summary

Avoid panicking upon `Disconnected` `Address`es.

## Context

In an actor system, it is possible to hold an `Address` to an actor that is currently not running and can thus not receive messages.
In such a case, `xtra` returns a `Disconnected` error from the various `send` functions.

Even though it appears that some of our actors may always be alive (like `wallet::Actor` or `monitor::Actor`), it can still happen that they disconnect if a panic occurs within that actor.
A panic will abort the event loop within the context and the task inside the tokio runtime stops, dropping the actor and rendering the `Address` disconnected.

In such a case, panicking upon not being able to `send` to an address will trigger a domino effect by panicking the event loop of the current actor, slowly rendering the entire application unresponsive as all actors fall over.
Eventually, we will introduce a supervisor pattern to catch unexpected shutdowns of certain actors and restart them afterwards.
As a first step towards a more resilient system, we must not panic upon receiving `Disconnected` from any of the `send` functions.

The only exception to this rule is for retrieving an address to ourselves from _within_ a handler.
At that point, we know that the actor must be alive otherwise we would not be executing the current handler.
