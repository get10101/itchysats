# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

- Maker only: Add new metric under `{maker_url}/api/metrics` which shows the quantity of total positions

## [0.4.12] - 2022-04-26

### Changed

- Allow to run the `taker` binary without parameters: Default the bitcoin-network subcommand to `mainnet` and apply defaults for the `mainnet` and `testnet` maker.
- Change the finality confirmations for spend transactions to `3` confirmations instead of `1`.
- Move `Closed` CFDs to a separate database table freeing up space.
  This is a performance improvement for the startup of the application.
  Note that the database file will not change in size, but the space will be re-used internally.
- Performance improvements with adding cache and optimized loading for different actors.
- Add link to the internal wallet when there is not enough balance to open a new position.
- Improve wire log messages for better context for connection logs.

### Fixed

- An issue where CET finality was not monitored if we restarted the daemon before the CET was confirmed.
  This was fixed by adding CET monitoring to the restart behaviour of the monitor actor.
- An issue where the taker daemon lost connection to the maker because the maker did not send heartbeats due to the maker's connection actor being blocked.
  This was fixed by removing all blocking calls from the connection actors in both taker and maker which should result in a more stable connection handling overall.
- An issue where the `commit_tx` URL was not displayed for a `force` close scenario.

## [0.4.11] - 2022-04-04

### Changed

- Return a 500 Internal Server Error for `/api/cfds` whilst doing the initial load from the database.
  Previously, an empty list was returned, making clients think the database is empty.
  [PR 1717](https://github.com/itchysats/itchysats/pull/1717)

### Fixed

- Severe performance issues caused by redundant re-creation of aggregates.
  Aggregates will now be cached and only new events will be applied.
  This makes anything database related much more performant.
  [PR 1722](https://github.com/itchysats/itchysats/pull/1722)

## [0.4.10] - 2022-03-28

- Add a banner celebrating us pitching at Bitcoin 2022 conference in Miami.

## [0.4.9] - 2022-03-28

## [0.4.8] - 2022-03-24

## [0.4.7] - 2022-02-28

## [0.4.6] - 2022-02-23

## [0.4.3] - 2022-02-14

- Fix a potential deadlock in taker after losing connection to a maker by introducing timeouts for reading and writing to TCP socket.

## [0.4.2] - 2022-02-08

## [0.4.1] - 2022-02-04

## [0.4.0] - 2022-01-31

### Added

- Off-chain perpetual CFD rollover.
  Hourly, the taker sends a rollover request to the maker for each open position.
  The maker can accept or reject a rollover proposal.
  Upon acceptance the taker and maker collaboratively agree on an oracle price event further in the future and generate new payout transactions accordingly.
  The previous payout transactions are invalidated.
  The new payout transactions spend from the same lock transaction, so the rollover happens off-chain.
  In case a maker rejects a rollover request from a taker the old oracle price event and payout transactions stay in place.
- Basic authentication for the web interface of the `taker` binary.
  A password is now required to access the web interface.
  If not set via `--password`, one will be derived from the seed file and displayed in the start-up logs.

### Changed

- Username for HTTP authentication to `itchysats`.

## [0.3.6] - 2022-01-05

Backport of <https://github.com/itchysats/itchysats/pull/1016>.

## [0.3.5] - 2021-12-30

Backport of <https://github.com/itchysats/itchysats/pull/987>.

## [0.3.4] - 2021-12-30

### Added

- Proposed settlement price in case of an incoming settlement proposal.

## [0.3.3] - 2021-12-29

Backport of <https://github.com/itchysats/itchysats/pull/982>.

Backport of <https://github.com/itchysats/itchysats/pull/972>.

Backport of <https://github.com/itchysats/itchysats/pull/977>.

Backport of <https://github.com/itchysats/itchysats/pull/971>.

## [0.3.2] - 2021-12-21

Backport <https://github.com/itchysats/itchysats/pull/927> in an attempt to fix <https://github.com/itchysats/itchysats/issues/759>.

## [0.3.1] - 2021-12-20

Backport <https://github.com/itchysats/itchysats/pull/924> in an attempt to fix <https://github.com/itchysats/itchysats/issues/759>.

## [0.3.0] - 2021-12-09

Initial release for mainnet.

[Unreleased]: https://github.com/itchysats/itchysats/compare/0.4.12...HEAD
[0.4.12]: https://github.com/itchysats/itchysats/compare/0.4.11...0.4.12
[0.4.11]: https://github.com/itchysats/itchysats/compare/0.4.10...0.4.11
[0.4.10]: https://github.com/itchysats/itchysats/compare/0.4.9...0.4.10
[0.4.9]: https://github.com/itchysats/itchysats/compare/0.4.8...0.4.9
[0.4.8]: https://github.com/itchysats/itchysats/compare/0.4.7...0.4.8
[0.4.7]: https://github.com/itchysats/itchysats/compare/0.4.6...0.4.7
[0.4.6]: https://github.com/itchysats/itchysats/compare/0.4.3...0.4.6
[0.4.2]: https://github.com/itchysats/itchysats/compare/0.4.1...0.4.2
[0.4.1]: https://github.com/itchysats/itchysats/compare/0.4.0...0.4.1
[0.4.0]: https://github.com/itchysats/itchysats/compare/0.3.6...0.4.0
[0.3.6]: https://github.com/itchysats/itchysats/compare/0.3.5...0.3.6
[0.3.5]: https://github.com/itchysats/itchysats/compare/0.3.4...0.3.5
[0.3.4]: https://github.com/itchysats/itchysats/compare/0.3.3...0.3.4
[0.3.3]: https://github.com/itchysats/itchysats/compare/0.3.2...0.3.3
[0.3.2]: https://github.com/itchysats/itchysats/compare/0.3.1...0.3.2
[0.3.1]: https://github.com/itchysats/itchysats/compare/0.3.0...0.3.1
[0.3.0]: https://github.com/itchysats/itchysats/compare/d12e04d4954deb2ee9ebdc9...0.3.0
