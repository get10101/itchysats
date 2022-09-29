/* eslint global-require: off, no-console: off, promise/always-return: off */

/**
 * This module executes inside of electron's main process. You can start
 * electron renderer process from here and communicate with the other processes
 * through IPC.
 *
 * When running `npm run build` or `npm run build:main`, this file is compiled to
 * `./src/main.js` using webpack. This gives us some performance wins.
 */
import { app, BrowserWindow, ipcMain, net, shell } from "electron";
import log from "electron-log";
import path from "path";
import MenuBuilder from "./menu";
import { resolveHtmlPath } from "./util";
// eslint-disable-next-line import/no-unresolved
const { itchysats } = require("../../index.node");

let mainWindow: BrowserWindow | null = null;

ipcMain.on("ipc-example", async (event, arg) => {
    const msgTemplate = (pingPong: string) => `IPC test: ${pingPong}`;
    console.log(msgTemplate(arg));
    event.reply("ipc-example", msgTemplate("pong"));
});

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
    log.info("Calling url");
    const request = net.request("http://127.0.0.1:8000");
    request.on("response", () => {
        if (!mainWindow) {
            log.error("Main window not defined. Terminating");
            return;
        }
        log.log("ItchySats is available!");
        log.debug("Loading ItchySats into browser window.");
        mainWindow
            .loadURL("http://127.0.0.1:8000")
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

app
    .whenReady()
    .then(async () => {
        await createWindow();

        log.debug("Waiting for ItchySats to become available.");

        process.env.ITCHYSATS_ENV = "electron";

        const dataDir = app.isPackaged ? app.getPath("userData") : app.getAppPath();
        const network = app.isPackaged ? "mainnet" : "testnet";
        log.info("Starting ItchySats ...");
        log.info(`Network: ${network}`);
        log.info(`Data Dir: ${dataDir}`);
        log.info(`Platform: ${process.platform}`);

        // start itchysats taker
        // eslint-disable-next-line promise/no-nesting
        itchysats(network, dataDir)
            .then(() => {
                log.info("Stopped ItchySats.");
            })
            .catch((error: Error) => log.error(error));

        app.on("activate", () => {
            // On macOS it's common to re-create a window in the app when the
            // dock icon is clicked and there are no other windows open.
            if (mainWindow === null) createWindow();
        });
    })
    .catch(console.log);
