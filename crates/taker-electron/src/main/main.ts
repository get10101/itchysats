/**
 * This module executes inside of electron's main process. You can start
 * electron renderer process from here and communicate with the other processes
 * through IPC.
 *
 * When running `npm run build` or `npm run build:main`, this file is compiled to
 * `./src/main.js` using webpack. This gives us some performance wins.
 */
import { app, BrowserWindow, shell } from "electron";
import log from "electron-log";
import path from "path";
import { Endpoint } from "./endpoint";
import MenuBuilder from "./menu";
import { resolveHtmlPath } from "./util";
/* eslint @typescript-eslint/no-var-requires: "off" */
const { itchysats } = require("../../index.node");

let mainWindow: BrowserWindow | null = null;

if (process.env.NODE_ENV === "production") {
    const sourceMapSupport = require("source-map-support");
    sourceMapSupport.install();
}

const createWindow = async (endpoint: Endpoint) => {
    const RESOURCES_PATH = app.isPackaged
        ? path.join(process.resourcesPath, "assets")
        : path.join(__dirname, "../../assets");

    const getAssetPath = (...paths: string[]): string => {
        return path.join(RESOURCES_PATH, ...paths);
    };

    mainWindow = new BrowserWindow({
        show: false,
        width: 1124,
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

    endpoint.wait((url: string) => {
        if (!mainWindow) {
            log.error("Main window not defined. Terminating");
            return;
        }
        log.log("ItchySats is available!");
        log.debug("Loading ItchySats into browser window.");
        mainWindow.loadURL(url).then(() => {
            log.info("Successfully loaded ItchySats!");
        }).catch((error: Error) => log.error(`Failed to load ItchySats! Error: ${error}`));
    });
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

app.whenReady().then(async () => {
    const endpoint = await Endpoint.create();

    console.log("created endpoint");
    await createWindow(endpoint);
    log.debug("Waiting for ItchySats to become available.");

    process.env.ITCHYSATS_ENV = "electron";

    const dataDir = app.isPackaged ? app.getPath("userData") : app.getAppPath();
    const network = app.isPackaged ? "mainnet" : "testnet";

    log.info("Starting ItchySats ...");
    log.info(`Network: ${network}`);
    log.info(`Data Dir: ${dataDir}`);
    log.info(`Platform: ${process.platform}`);
    log.info(`Endpoint: ${endpoint.url}`);

    // start itchysats taker on random ports
    itchysats(network, dataDir, endpoint.port)
        .then(() => log.info("Stopped ItchySats."))
        .catch((error: Error) => log.error(error));

    app.on("activate", () => {
        // On macOS it's common to re-create a window in the app when the
        // dock icon is clicked and there are no other windows open.
        if (mainWindow === null) createWindow(endpoint);
    });
}).catch(console.log);
