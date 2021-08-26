import { resolve } from "path";

import reactRefresh from "@vitejs/plugin-react-refresh";
import { defineConfig } from "vite";

// https://vitejs.dev/config/
export default defineConfig({
    plugins: [
        (
            process.env.NODE_ENV !== "test"
                ? [reactRefresh()]
                : []
        ),
    ],
    build: {
        rollupOptions: {
            input: {
                maker: resolve(__dirname, "maker.html"),
                taker: resolve(__dirname, "taker.html"),
            },
        },
    },
    server: {
        open: "/maker",
    },
    // server: {
    //     proxy: {
    //         '/foo': 'http://localhost:4567',
    //         '/api': {
    //             target: 'http://jsonplaceholder.typicode.com',
    //             changeOrigin: true,
    //             rewrite: (path) => path.replace(/^\/api/, '')
    //         },
    //         // with RegEx
    //         '^/fallback/.*': {
    //             target: 'http://jsonplaceholder.typicode.com',
    //             changeOrigin: true,
    //             rewrite: (path) => path.replace(/^\/fallback/, '')
    //         },
    //         // Using the proxy instance
    //         '/api': {
    //             target: 'http://jsonplaceholder.typicode.com',
    //             changeOrigin: true,
    //             configure: (proxy, options) => {
    //                 // proxy will be an instance of 'http-proxy'
    //             }
    //         }
    //     }
    // }
});
