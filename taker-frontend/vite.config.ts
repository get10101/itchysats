import reactRefresh from "@vitejs/plugin-react-refresh";
import { resolve } from "path";
import { defineConfig } from "vite";

// https://vitejs.dev/config/
export default defineConfig({
    plugins: [
        process.env.NODE_ENV !== "test"
            ? [reactRefresh()]
            : [],
    ],
    build: {
        rollupOptions: {
            input: resolve(__dirname, `index.html`),
        },
        outDir: `dist/taker`,
    },
    server: {
        port: 3000,
        proxy: {
            "/api": "http://localhost:8000",
            "/alive": "http://localhost:8000",
        },
    },
});
