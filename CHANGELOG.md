# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

## [0.3.0] - 2021-12-09

Initial release for mainnet.

[Unreleased]: https://github.com/itchysats/itchysats/compare/0.3.0...HEAD
[0.3.0]: https://github.com/itchysats/itchysats/compare/d12e04d4954deb2ee9ebdc9...0.3.0
