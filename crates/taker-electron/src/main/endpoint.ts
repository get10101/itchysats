import { net } from "electron";
import log from "electron-log";
import { AddressInfo } from "net";
import nodenet from "node:net";

const DEFAULT_PORT = 8000;
const MAX_RETRIES = 5;
const TIMEOUT = 500; // in miliseconds
const MIN_PORT = 10_000;
const MAX_PORT = 65_535;
const HOST = "127.0.0.1";

export class Endpoint {
    public url: string;

    constructor(public port: number) {
        this.url = `http://${HOST}:${port}`;
    }

    wait(onReady: (endpoint: string) => void) {
        const alive = (timeout: number) => {
            log.info(`Probing if ItchySats is alive at ${this.url}`);
            const request = net.request(this.url);
            request.on("response", () => onReady(this.url));
            request.on("error", (error) => {
                log.warn("Could not connect to ItchySats.");
                log.error(error);
                setTimeout(alive, timeout, timeout + timeout);
            });
            request.end();
        };

        setTimeout(alive, TIMEOUT, TIMEOUT);
    }

    static create(maxRetries = MAX_RETRIES, port = DEFAULT_PORT): Promise<Endpoint> {
        // retry checking random port by the given amount of max retries.
        return new Promise<Endpoint>((resolve, reject) => {
            const server = nodenet.createServer();
            server.unref();
            server.on("error", (err) => {
                log.info(`Port: ${port} is not available retrying another random port. Retries: ${maxRetries}`);
                if (maxRetries <= 0) {
                    log.error(`Reached max amount of retries. quitting.`);
                    return reject(err);
                }
                return resolve(Endpoint.create(
                    maxRetries - 1,
                    Math.floor(Math.random() * (MAX_PORT - MIN_PORT + 1) + MIN_PORT),
                ));
            });
            log.debug(`Trying port: ${port}`);
            server.listen({ port, host: HOST }, () => {
                const { port } = <AddressInfo> server.address();
                server.close(() => {
                    log.debug(`Found open port: ${port}!`);
                    return resolve(new Endpoint(port));
                });
            });
        });
    }
}
