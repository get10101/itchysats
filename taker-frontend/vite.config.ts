import react from "@vitejs/plugin-react";
import { resolve } from "path";
import { defineConfig } from "vite";

// https://vitejs.dev/config/
export default defineConfig({
    plugins: [react()],
    build: {
        rollupOptions: {
            input: resolve(__dirname, `index.html`),
        },
        outDir: `dist/taker`,
    },
    server: {
        proxy: {
            "/api": "http://localhost:8000",
        },
    },
});
