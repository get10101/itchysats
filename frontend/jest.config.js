module.exports = {
    preset: "vite-jest",
    setupFilesAfterEnv: ["<rootDir>/jest.setup.js"],
    testMatch: [
        "<rootDir>/src/**/*.test.{js,jsx,ts,tsx}",
    ],
    testEnvironment: "jest-environment-jsdom",
};
