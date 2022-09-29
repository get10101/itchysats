# ItchySats Electron

This project uses [neon](https://neon-bindings.com/docs/introduction) to embed the taker Rust daemon into a Node.Js library.

## How to build the project

The following command compiles all rust dependencies and embeds the taker daemon into a Node.JS library, creating an index.node file which can be required by any javascript application.

```bash
yarn install
```

## How to start the project

The following command will start the taker daemon in an electron environment.

```bash
yarn start
```

Note: make sure you've built `taker-frontend` in [../taker-frontend](../taker-frontend).

## How to package the project

The following command will package the itchysats electron app based on your host platform for the architectures x64 and arm64.

```bash
yarn package
```

## FAQ

**Q:** Where do I find the application logs?

**A:** Depending on your platform itchysats stores the application logs in the following directories.

- macos: ~/Library/Logs/itchysats/main.log
- windows: %USERPROFILE%\AppData\Roaming\itchysats\logs\main.log
- linux: ~/.config/itchysats/logs/main.log

## 

**Q:** Where do I find my wallet?

**A:** Depending on your platform itchysats stores the user data in the following directories.

- macos: ~/Library/Application Support/itchysats
- windows: %APPDATA%\itchysats
- linux: ~/.config/itchysats
