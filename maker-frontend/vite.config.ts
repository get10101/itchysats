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
        outDir: `dist/maker`,
    },
    server: {
        port: 3001,
        proxy: {
            "/api": `http://localhost:8001`,
            "/alive": `http://localhost:8001`,
        },
    },
});
