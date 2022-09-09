# FAQ - Frequently Asked Questions

<!-- markdown-toc start - Don't edit this section. Run M-x markdown-toc-refresh-toc -->

**Table of Contents**

- [FAQ - Frequently Asked Questions](#faq---frequently-asked-questions)
  - [Installation](#installation)
  - [Trading](#trading)
  - [Wallet](#wallet)
  - [Contact us](#contact-us)

<!-- markdown-toc end -->

## Installation

Q: Do I need to use Umbrel to use ItchySats?\
A: Not at all! While [Umbrel](https://getumbrel.com) is a convenient way to use ItchySats (one-click install from the Umbrel App Store), it is possible to try out ItchySats with one of the following methods:

- download the latest ItchySats taker binary straight from [github releases page](https://github.com/itchysats/itchysats/releases/latest).
- use the official ItchySats [Docker container](https://github.com/itchysats/itchysats/pkgs/container/itchysats%2Ftaker)

Q: I see a lot of binaries, which one should I choose?\
A: As an end-user, you might want to use the `taker` application, as this allows to open CFDs against the ItchySats maker. The `maker` binary is targeted towards market-makers who want to offer CFDs (it is not officially supported yet, but it be in the future). The flavour of the application depends on the operating system you use - we currently provide binaries for linux (x64 and ARM) and Darwin/macOS (Intel-based).

Q: Do you support Windows?\
A: Whilst we do not build Windows binaries directly, you should still be able to use the official ItchySats Docker container.

Q: I installed the app, but I can't log in - it asks me for password?
A: Your password is derived from the `taker_seed` file, and the credentials can be seen it the taker logs right after startup.

Q: Can I use ItchySats on mobile?
A: Whilst we currently don't provide iOS or Android apps, our UI is mobile-friendly. This means that you can connect from your mobile browser to the machine running ItchySats app. At the moment the easiest way to do it is to run Itchysats on [Umbrel](https://getumbrel.com). tip: If you use [Tailscale](https://tailscale.com/) app on your Umbrel node, you can trade on-the-go 😃 (without needing to be on the same local network).
Umbrel and RaspiBlitz also expose ItchySats with an onion address, which allows connecting remotely from mobile via [Tor](https://www.torproject.org/).

## Trading

Q: How long does a CFD last?\
A: CFDs opened on ItchySats expire after 24h, however every hour the contract is automatically extended for another hour. This means you have a perpetual CFD that stays open until you decide to close it.

Q: Do I need to keep the app running to have perpetual CFDs?\
A: It is recommended to have the app running in the background when you have open CFDs in order to automatically extend their lifetime.

## Wallet

Q: Can I use my existing wallet / umbrel wallet?\
A: Not yet. We've got it on our roadmap, but for the time being the app has its own inbuilt wallet into which you can transfer funds. It is available in the "Wallet" tab.

Q: Where is the wallet tab? I can't find the wallet!\
A: The wallet tab can be found after clicking the hamburger menu in the top left corner of the app.

Q: How can I back up my ItchySats wallet?\
A: On Umbrel the wallet is derived from the Umbrel app-seed, so you don't need to backup anything in addition. Binary and docker containers users should backup the `taker_seed` file that is used to derive the wallet.

## Seed

Q: What's `taker_seed`?
A: A file that is used to derive both the wallet keys and the credentials for the ItchySats UI. It is generated on the first launch, It is essential to back it up and store it securely, especially when using ItchySats on mainnet.

Q: Can I restore my ItchySats wallet balance if I copy `taker_seed` again?
A: Certainly. New `taker_seed` file is only generated if there is none present; if you overwrite a new seed with an old one, you should see you balance again after you start ItchySats again. It is recommended to go through the backup/restore procedure at least once to be safe that nothing is lost.

Q: Can I restore my ItchySats trade history if I copy `taker_seed` again?
A: Unfortunately not. In order to backup and restore your trading history, you should copy the `taker.sqlite` database file. Note that the database stores no information about your wallet, so copying `taker_seed` is still required.

## Contact us

In case none of the above questions match the problem you're having, please feel free to reach out to us via one of the following channels:

- [GitHub](https://github.com/itchysats/itchysats/discussions)
- [Telegram](https://t.me/joinchat/ULycH50PLV1jOTI0)
- [Twitter](https://twitter.com/itchysats)
- e-mail: hello (at) itchysats.network
