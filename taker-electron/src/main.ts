import { app, BrowserWindow, net } from "electron";
import * as logger from "electron-log";
import * as path from "path";
/* eslint-disable @typescript-eslint/no-var-requires */
const { itchysats } = require("../index.node");

export default class Main {
    static mainWindow: Electron.BrowserWindow;

    // Quit when all windows are closed, except on macOS. There, it's common
    // for applications and their menu bar to stay active until the user quits
    // explicitly with Cmd + Q.
    private static onWindowAllClosed() {
        if (process.platform !== "darwin") {
            app.quit();
        }
    }

    // This method will be called when Electron has finished
    // initialization and is ready to create browser windows.
    // Some APIs can only be used after this event occurs.
    private static onReady() {
        logger.debug("Waiting for ItchySats to become available.");
        Main.createWindow();

        process.env.ITCHYSATS_ENV = "electron";

        const dataDir = app.isPackaged ? app.getPath("userData") : app.getAppPath();
        const network = app.isPackaged ? "mainnet" : "testnet";
        logger.info("Starting ItchySats ...");
        logger.info(`Network: ${network}`);
        logger.info(`Data Dir: ${dataDir}`);

        // start itchysats taker
        itchysats(network, dataDir).then(() => {
            logger.info("Stopped ItchySats.");
        }).catch((error: Error) => logger.error(error));

        app.on("activate", function() {
            // On macOS it's common to re-create a window in the app when the
            // dock icon is clicked and there are no other windows open.
            if (BrowserWindow.getAllWindows().length === 0) {
                Main.createWindow();
            }
        });
    }

    private static createWindow() {
        logger.info("Creating window.");
        // Create the browser window.
        Main.mainWindow = new BrowserWindow({
            width: 1250,
            height: 1000,
            title: "ItchySats",
            icon: path.join(__dirname, "logo.icns"),
        });
        app.setName("ItchySats");

        // load loading screen
        Main.mainWindow.loadFile("index.html");

        setTimeout(Main.alive, 500, 200);
    }

    // checks if the itchysats taker user interface is available.
    private static alive(timeout: number) {
        const request = net.request("http://127.0.0.1:8000");
        request.on("response", () => {
            logger.log("ItchySats is available!");
            logger.debug("Loading ItchySats into browser window.");
            Main.mainWindow.loadURL("http://127.0.0.1:8000").then(() => {
                logger.info("Successfully loaded ItchySats!");
            }).catch((error: Error) => logger.error(`Failed to load ItchySats! Error: ${error}`));
        });
        request.on("error", (error) => {
            logger.warn("Could not connect to ItchySats.");
            logger.error(error);
            setTimeout(Main.alive, timeout, timeout + timeout);
        });
        request.end();
    }

    public run() {
        app.on("window-all-closed", Main.onWindowAllClosed);
        app.on("ready", Main.onReady);
    }
}

new Main().run();
