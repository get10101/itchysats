/* eslint global-require: off, no-console: off, promise/always-return: off */

/**
 * This module executes inside of electron's main process. You can start
 * electron renderer process from here and communicate with the other processes
 * through IPC.
 *
 * When running `npm run build` or `npm run build:main`, this file is compiled to
 * `./src/main.js` using webpack. This gives us some performance wins.
 */
import { app, BrowserWindow, net, shell } from "electron";
import log from "electron-log";
import nodenet from "node:net";
import path from "path";
import MenuBuilder from "./menu";
import { resolveHtmlPath } from "./util";
import {AddressInfo} from "net";
// eslint-disable-next-line import/no-unresolved
const { itchysats } = require("../../index.node");

let mainWindow: BrowserWindow | null = null;
let itchySatsPort = 8000;

if (process.env.NODE_ENV === "production") {
    const sourceMapSupport = require("source-map-support");
    sourceMapSupport.install();
}

const isDebug = process.env.NODE_ENV === "development" || process.env.DEBUG_PROD === "true";

if (isDebug) {
    require("electron-debug")();
}

const installExtensions = async () => {
    const installer = require("electron-devtools-installer");
    const forceDownload = !!process.env.UPGRADE_EXTENSIONS;
    const extensions = ["REACT_DEVELOPER_TOOLS"];

    return installer
        .default(
            extensions.map((name) => installer[name]),
            forceDownload,
        )
        .catch(console.log);
};

const alive = (timeout: number) => {
    log.info(`Probing if ItchySats is alive at http://127.0.0.1:${itchySatsPort}`);
    const request = net.request(`http://127.0.0.1:${itchySatsPort}`);
    request.on("response", () => {
        if (!mainWindow) {
            log.error("Main window not defined. Terminating");
            return;
        }
        log.log("ItchySats is available!");
        log.debug("Loading ItchySats into browser window.");
        mainWindow
            .loadURL(`http://127.0.0.1:${itchySatsPort}`)
            .then(() => {
                log.info("Successfully loaded ItchySats!");
            })
            .catch((error: Error) => log.error(`Failed to load ItchySats! Error: ${error}`));
    });
    request.on("error", (error) => {
        log.warn("Could not connect to ItchySats.");
        log.error(error);
        setTimeout(alive, timeout, timeout + timeout);
    });
    request.end();
};

const createWindow = async () => {
    if (isDebug) {
        await installExtensions();
    }

    const RESOURCES_PATH = app.isPackaged
        ? path.join(process.resourcesPath, "assets")
        : path.join(__dirname, "../../assets");

    const getAssetPath = (...paths: string[]): string => {
        return path.join(RESOURCES_PATH, ...paths);
    };

    mainWindow = new BrowserWindow({
        show: false,
        width: 1024,
        height: 728,
        icon: getAssetPath("icon.png"),
        webPreferences: {
            sandbox: false,
        },
    });

    // To loading frontend before ItchySats is fully loaded
    mainWindow.loadURL(resolveHtmlPath("index.html"));

    mainWindow.on("ready-to-show", () => {
        if (!mainWindow) {
            throw new Error("\"mainWindow\" is not defined");
        }
        if (process.env.START_MINIMIZED) {
            mainWindow.minimize();
        } else {
            mainWindow.show();
        }
    });

    mainWindow.on("closed", () => {
        mainWindow = null;
    });

    const menuBuilder = new MenuBuilder(mainWindow);
    menuBuilder.buildMenu();

    // Open urls in the user's browser
    mainWindow.webContents.setWindowOpenHandler((edata) => {
        shell.openExternal(edata.url);
        return { action: "deny" };
    });

    setTimeout(alive, 500, 500, 500);
};

/**
 * Add event listeners...
 */

app.on("window-all-closed", () => {
    // Respect the OSX convention of having the application in memory even
    // after all windows have been closed
    if (process.platform !== "darwin") {
        app.quit();
    }
});

// retry checking random `itchySatsPort` by the given amount of max retries.
const retry = (
    maxRetries: number,
    checkPort: (port: number) => Promise<number>,
    port: number,
): Promise<number> => {
    const minPort = 10_000;
    const maxPort = 65_535;

    return checkPort(port).catch((err) => {
        log.info(
            `Port: ${port} is not available retrying another random port. Retries: ${maxRetries}`,
        );
        if (maxRetries <= 0) {
            log.error(`Reached max amount of retries. quitting.`);
            throw err;
        }
        return retry(
            maxRetries - 1,
            checkPort,
            Math.floor(Math.random() * (maxPort - minPort + 1) + minPort),
        );
    });
};

// checks if the provided `itchySatsPort` is already taken on the localhost.
const checkAvailablePort = (port: number): Promise<number> =>
    new Promise((resolve, reject) => {
        const server = nodenet.createServer();
        server.unref();
        server.on("error", reject);
        log.debug(`Trying port: ${port}`);
        server.listen({ port, host: "127.0.0.1" }, () => {
            const { port: serverPort } = <AddressInfo> server.address();
            server.close(() => {
                log.debug(`Found open port: ${serverPort}!`);
                resolve(serverPort);
            });
        });
    });

app
    .whenReady()
    .then(async () => {
        await createWindow();
        log.debug("Waiting for ItchySats to become available.");

        process.env.ITCHYSATS_ENV = "electron";

        const dataDir = app.isPackaged ? app.getPath("userData") : app.getAppPath();
        const network = app.isPackaged ? "mainnet" : "testnet";

        // try to pick the standard `itchySatsPort` and retry random ports if not available.
        itchySatsPort = await retry(5, checkAvailablePort, 7113);

        log.info("Starting ItchySats ...");
        log.info(`Network: ${network}`);
        log.info(`Data Dir: ${dataDir}`);
        log.info(`Platform: ${process.platform}`);
        log.info(`ItchySats Port: ${itchySatsPort}`);

        // start itchysats taker on random ports
        // eslint-disable-next-line promise/no-nesting
        itchysats(network, dataDir, itchySatsPort)
            .then(() => log.info("Stopped ItchySats."))
            .catch((error: Error) => log.error(error));

        app.on("activate", () => {
            // On macOS it's common to re-create a window in the app when the
            // dock icon is clicked and there are no other windows open.
            if (mainWindow === null) createWindow();
        });
    })
    .catch(console.log);
