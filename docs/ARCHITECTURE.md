# Architecture

This document outlines the architecture of this software and the rationale that went into it.
Architectural invariants more often than not defines the _absence_ of something.
Seeing something that is not there is hard, which is why we make an effort to document these things in here.

We define _components_ as software elements that talk to each other at runtime.
The component split does not necessarily reflect the source code split of the software.
For example, the library is embedded in both daemon but only exists once as a source code element.

## Overview

The application is split into several components:

- A web frontend for the taker
- A taker daemon
- A web frontend for the maker
- A maker daemon

At runtime, both daemons embed their frontend and serve it via HTTP.

On a source-code level, we split into:

- A library defining the core, cryptographic protocol
- A _crate_ defining the two daemons
- Two React-based frontend projects

## Invariants

### Event-based communication between frontend and backend

Each frontend subscribes to a SSE-based feed from the backend.
Each event notifies the frontend about a change in the backend's state.
This state change can either be a result of a user interaction or incoming network communication.

User interaction MUST NOT directly change the state displayed to the user.

Instead, we maintain a cycle of:

1. User interaction triggers POST request to backend
1. Backend state changes
1. State change emits update on SSE feed
1. Event on SSE triggers re-render in application

As a result of this invariant, we can be sure that any state change is accurately reflected in the frontend, regardless of how it was triggered.
It also makes the frontend very thin and therefore more predictable.

### Update local state first

To keep our UI stateless and responsive, we always update the local state of our daemon first (and emit an event for it).
Only once the local state is updated, we engage with other systems like the maker/taker daemon.

Applying state changes locally first allows us to record the user's intention and provide instant (UI) feedback that we are working on making it happen.
Even if the user restarts the entire application, we can pick up where we left of and finish what we were meant to be doing.

### Append only database

The database structure is append only.
We never update / overwrite existing state to make sure we don't ever loose data due to bugs in state transitions.

### Library only exposes pure transition functions rather than a state machine

The protocol implemented in the library can be thought of as a state machine that is pushed forward by each party.
To remain flexible in how the protocol is used, the library MUST only expose pure functions to go from one state to the other rather than representing the actual states itself.
This allows applications on top of shape their states and messages as they wish, only reaching into the library for doing the heavy lifting of cryptography and other protocol-specific functionality.
