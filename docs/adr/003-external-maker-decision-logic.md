# ADR 003 - Keep maker decision logic external to the system

```
Author: @thomaseizinger
Date: 29/12/2021
Status: Active
```

## Summary

Keep the maker's business decision logic for sending out offers or accepting / declining incoming orders outside the system.

## Context

There are multiple aspects contributing to this decision.

### 1. Business logic is financially valuable

All of itchysats is open-source to allow for auditability and independent verification.
The business logic for when to send out offers and which orders to accept is financially valuable.
It is part of the maker's business plan and the system should allow for this to be closed source or otherwise not easily accessible.

### 2. Business logic changes at a different pace

The code that comprises itchysats will change with the advent of new features and / or technical improvements.
More often than not, the business logic on when to create a new offer and which orders to accept changes in a completely different way.
The rule "put together what changes together" suggests to us that business logic and the rest of itchysats should _not_ be together.

### 3. Business logic is complicated to automate

Automating this business logic is non-trivial and often involves other, external system for arbitraging or hedging.
Adding this code to the core itchysats system will introduce additional complexity through dependencies and the addition of new APIs.
For the same reasons as (2), changes to this business logic often require changing the signature of these APIs to feed in more parameters.

### Summary

The above list is likely not complete but good enough to decide that this business logic should be implemented _outside_ of itchysats.
The technical design on how this is decoupled (plugins, HTTP APIs, etc) is not important and deliberately omitted from this decision.
