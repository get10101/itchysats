export {};

declare global {
    interface Window {
        itchysats: {
            load: () => void;
        };
    }
}
