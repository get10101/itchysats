# Hermes Roadmap

## MVP

The Minimal Viable Product's goal is to showcase that non-custidial CFD trading on Bitcoin is possible.

## Scope

### Roles

- **Taker**: Cannot create and publish orders, but just takes what is published by a Maker
- **Maker**: Can create and publish orders
- **Oracle**: Signs and publishes attested prices at specific points in time

### In scope

For the MVP there is only one Maker that takes the selling side and creates sell orders.
The maker does not do any automation.
The maker dictates the price.

A user is always in the role of a taker.
The user has a simple user interface and can take the maker's order there.
The taker can specify a quantity, the leverage is fixed to `x5`.
For the MVP the leverage is fixed to `x5` for both sell and buy orders.
The oracle is needed for attestation of prices at a certain point in time.
The oracle is to be run by a separate party that is neither the taker nor the maker.

Overview sequence diagram:

![MVP sequence diagram](http://www.plantuml.com/plantuml/proxy?cache=no&src=https://raw.githubusercontent.com/comit-network/hermes/b7778f0556d1cbfd578aa28042beb24c7f13d5a3/docs/asset/mvp_sequence_diagram.puml)

Constraints:

♻️ ... expected to be reusable for later iterations

- ♻️ Protocol: Non-custodial using DLCs
  - On-chain (testnet)
- Software Setup
  - Taker
    - Local running daemon that exposes API + web-interface for UI
    - Can take a sell order (represents the buy side)
      - Specify quantity
      - (fixed leverage of `x5`)
  - Maker
    - Can create a sell order (represents the sell side)
    - Sell order publication is done manually
    - Take requests are accepted manually
  - ♻️ Oracle

### Out of scope

- Anonymity
- Orderbook
- Multiple makers

