# ADR 013 - Allow all actors to load and hydrate a CFD, not just the `{taker,maker}_cfd::Actor`

```
Author: @thomaseizinger
Date: 29/12/2021
Status: Active
```

## Summary

Load and hydrate a CFD from the database wherever necessary, avoiding proxying information through to the `{taker,maker}_cfd::Actor`.

## Context

Currently, only the `{taker,maker}_cfd::Actor` have access to the database.
This constraint was necessary prior to event sourcing because all business logic about updating a CFD's state was contained within that actor.
With the introduction of event sourcing, this business logic is contained within the `Cfd` model.
As a result, we no longer need to send messages to the `{taker,maker}_cfd::Actor` to tell it to perform certain actions like using an attestion to decrypt a CET or recording that a certain transaction is confirmed.

By relaxing the current restriction of who can load the CFD from the database, some communication between the actors like sending around attestations and monitoring events should become unnecessary.
