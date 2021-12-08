# Technical vision

In this document, I want to capture the mid term technical vision for the project.
Hopefully, this will make it clearer why we are working on certain parts of the codebase and which direction we should be transitioning them into.
This is a living document so check back regularly for updates!

## Resilience

### Always available UI

Ideally, starting the application is non-fallible and always present a UI to the user.
The user should be able to see the "health" of certain subsystems (wallet, blockchain, maker-connection, database, ...) and potentially take action in fixing them or we take an automated action in attempting to fix them.

### Self-healing

Our application should be self-healing.
This means, if certain components fail (like the websocket connection to BitMex), we should be able to restart them.
Starting and stopping is a first-class feature of actors.
In an ideal world, we tie the lifetime of these processes to an actor and thus, restarting the actor will restart that subsystem.
As a consequence, other actors need to properly deal with the fact that sending a message to another actor might yield the `Disconnected` error.

## Network communication

### Multiplexing

Our current network stack is solely built on top of TCP which does not support multiplexing.
Some of the complexity in the codebase stems from this missing feature.
For example, because we don't have any multiplexing, the `connection::Actor` needs to know, where to route messages for contract-setups, rollovers or collaborative settlements.
If we had a network stack with multiplexing support, we could open a dedicated stream just for a single protocol and hand both ends of the stream to such an actor and the right messages would "magically" arrive at the right actor.

## Model

### Event sourcing

Use event-sourcing as the core building block for modelling the state changes to a CFD.
Changes to a CFD are done by executing a _command_ on the CFD.
Executing a _command_ will produce a _Domain Event_.
A _Domain Event_ represents a _state change_ in the system.
These events are sent to a dedicated actor which first persists them and then broadcasts them to other actors.
Other actors can then react to this state change and

- update the UI
- interact with downstream systems (electrum, oracle, ...)

![Image](https://www.planttext.com/api/plantuml/img/TP4nRiCm34LtdeBmdWjaA9AYxTOsG8REZCssAcHIa9WOkNqKowYb8Uj_JuB-rouPHJkF7i2SUSRkJRtNQNCEIBqvbOJVKKVa2ukb3Y1at_Kka1Xsdv7wV6ZVcyOEAQ7EGIjzaVTibJJDGIkzgxZCdVnKubZ2rZn4_UFvQPKP_iDMtilrtcEnIAuDVhsNEcRAC93HYHANB05aT_Eq2blyuAciWByK0WiFiE95JLiyqeNH55-U6ro6gMvfQ5da4LrcU8JNxhK1EvOXV-mD)

### Naming

Align all terminology in the codebase closer with financial terms.
See also https://github.com/itchysats/itchysats/discussions/829.
Current issues:

- Order vs offer
- "Settlement" interval
- Interest vs funding rate

### IO-free yet contained

The execution of the protocols for contract setup and rollover are [IO-free](https://sans-io.readthedocs.io/how-to-sans-io.html).
More details here: https://github.com/itchysats/itchysats/issues/698.
