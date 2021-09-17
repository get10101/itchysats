import reactRefresh from "@vitejs/plugin-react-refresh";
import { resolve } from "path";
import { defineConfig } from "vite";
import dynamicApp from "./dynamicApp";

const app = process.env.APP;

if (!app || (app !== "maker" && app !== "taker")) {
    throw new Error("APP environment variable needs to be set to `maker` `taker`");
}

const backendPorts = {
    "taker": 8000,
    "maker": 8001,
};

// https://vitejs.dev/config/
export default defineConfig({
    plugins: [
        (
            process.env.NODE_ENV !== "test"
                ? [reactRefresh()]
                : []
        ),
        dynamicApp(app),
    ],
    build: {
        rollupOptions: {
            input: resolve(__dirname, `index.html`),
        },
        outDir: `dist/${app}`,
    },
    server: {
        proxy: {
            "/api": `http://localhost:${backendPorts[app]}`,
        },
    },
});
