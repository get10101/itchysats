"use strict";
exports.__esModule = true;
var electron_1 = require("electron");
var logger = require("electron-log");
var path = require("path");
/* eslint-disable @typescript-eslint/no-var-requires */
var itchysats = require("../index.node").itchysats;
var Main = /** @class */ (function () {
    function Main() {
    }
    // Quit when all windows are closed, except on macOS. There, it's common
    // for applications and their menu bar to stay active until the user quits
    // explicitly with Cmd + Q.
    Main.onWindowAllClosed = function () {
        if (process.platform !== "darwin") {
            electron_1.app.quit();
        }
    };
    // This method will be called when Electron has finished
    // initialization and is ready to create browser windows.
    // Some APIs can only be used after this event occurs.
    Main.onReady = function () {
        logger.debug("Waiting for ItchySats to become available.");
        Main.createWindow();
        process.env.ITCHYSATS_ENV = "".concat(process.platform, "-electron");
        var dataDir = electron_1.app.isPackaged ? electron_1.app.getPath("userData") : electron_1.app.getAppPath();
        var network = electron_1.app.isPackaged ? "mainnet" : "testnet";
        logger.info("Starting ItchySats ...");
        logger.info("Network: ".concat(network));
        logger.info("Data Dir: ".concat(dataDir));
        logger.info("Platform: ".concat(process.platform));
        // start itchysats taker
        itchysats(network, dataDir).then(function () {
            logger.info("Stopped ItchySats.");
        })["catch"](function (error) { return logger.error(error); });
        electron_1.app.on("activate", function () {
            // On macOS it's common to re-create a window in the app when the
            // dock icon is clicked and there are no other windows open.
            if (electron_1.BrowserWindow.getAllWindows().length === 0) {
                Main.createWindow();
            }
        });
    };
    Main.createWindow = function () {
        logger.info("Creating window.");
        // Create the browser window.
        Main.mainWindow = new electron_1.BrowserWindow({
            width: 1250,
            height: 1000,
            title: "ItchySats",
            icon: path.join(__dirname, "logo.icns")
        });
        electron_1.app.setName("ItchySats");
        // load loading screen
        Main.mainWindow.loadFile("index.html");
        setTimeout(Main.alive, 500, 200);
    };
    // checks if the itchysats taker user interface is available.
    Main.alive = function (timeout) {
        var request = electron_1.net.request("http://127.0.0.1:8000");
        request.on("response", function () {
            logger.log("ItchySats is available!");
            logger.debug("Loading ItchySats into browser window.");
            Main.mainWindow.loadURL("http://127.0.0.1:8000").then(function () {
                logger.info("Successfully loaded ItchySats!");
            })["catch"](function (error) { return logger.error("Failed to load ItchySats! Error: ".concat(error)); });
        });
        request.on("error", function (error) {
            logger.warn("Could not connect to ItchySats.");
            logger.error(error);
            setTimeout(Main.alive, timeout, timeout + timeout);
        });
        request.end();
    };
    Main.prototype.run = function () {
        electron_1.app.on("window-all-closed", Main.onWindowAllClosed);
        electron_1.app.on("ready", Main.onReady);
    };
    return Main;
}());
exports["default"] = Main;
new Main().run();
//# sourceMappingURL=main.js.map