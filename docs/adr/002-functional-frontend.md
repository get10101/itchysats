# ADR 002 - Functional, reactive frontend

```
Author: @thomaseizinger
Date: 29/12/2021
Status: Active
```

## Summary

Design the frontend as a pure function of the backend's state, avoiding selective updates.
Additionally, the frontend should be reactive to state changes in the backend.

## Context

State management is hard and we certainly don't want to do it in JavaScript / TypeScript.
To avoid that, we implement the following design for the frontend:

1. Backend exposes an SSE endpoint, sending out the latest state upon changes.
2. Frontend subscribes to this SSE endpoint, replacing its local state upon every update.
3. Frontend triggers updates to the state via POST requests
4. Upon success, state changes are sent out via the SSE feed.

The alert reader might recognize this as the MVC pattern, applied across a network boundary with the frontend representing the view and the backend holding the controller and model.

Most importantly, the SSE feed must not send out state changes but the entire latest state.
Sending out state changes would require the frontend to know, how to apply such a change event.
That is exactly the kind of logic we want to avoid.
Instead, we want to send out the entire latest state (list of all CFDs, current wallet balance, etc).
When using a library like React, implementing a frontend like this becomes a piece of cake:

1. Subscribe to the SSE feed
2. Event handlers set the latest event as the current state
3. Changes in the current state trigger a re-render by React
