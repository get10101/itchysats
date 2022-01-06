# Architecture decision records

This directory contains all ADRs (architecture decision records) for itchysats.

## Records

- [ADR 001: Use ADRs](./001-use-adrs.md)
- [ADR 002: Functional, reactive frontend](./002-functional-frontend.md)
- [ADR 003: Keep maker decision logic external to the system](./003-external-maker-decision-logic.md)
- [ADR 004: Actors](./004-actors.md)
- [ADR 005: Use actors to model protocols](./005-actors-for-protocols.md)
- [ADR 006: Use actors for modelling resources](./006-actors-for-resources.md)
- [ADR 007: Use event sourcing for managing a CFD's state](./007-event-sourcing-for-cfds.md)
- [ADR 008: Use OCC to avoid race conditions](./008-occ-for-race-conditions.md)
- [ADR 009: Model "starting" a protocol as a domain event](./009-protocols-start-is-an-event.md)
- [ADR 010: Process manager for orchestration](./010-process-manager-for-orchestration.md)
- [ADR 011: Process manager must only fail on saving the event](./011-process-manager-no-fail.md)
- [ADR 012: Do not panic on actors being `Disconnected`](./012-no-panic-on-disconnected.md)
- [ADR 013: Allow all actors to load and hydrate a CFD, not just the `{taker,maker}_cfd::Actor`](./013-all-actors-can-hydrate-cfd.md)
