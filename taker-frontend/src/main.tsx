import { ChakraProvider } from "@chakra-ui/react";
import React from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter, Navigate, Outlet, Route, Routes } from "react-router-dom";
import { App } from "./App";
import useAuth, { AuthProvider } from "./authentication/useAuth";
import ChangePassword from "./ChangePassword";
import "./index.css";
import LoginPage from "./LoginPage";
import theme from "./theme";

const container = document.getElementById("root");
const root = createRoot(container!);

const PrivateRoutes = () => {
    const { authenticated, firstLogin } = useAuth();

    if (!authenticated) return <Navigate to="/login" />;
    if (firstLogin) return <Navigate to="/change-password" />;

    return <Outlet />;
};

root.render(
    <React.StrictMode>
        <ChakraProvider theme={theme}>
            <BrowserRouter>
                <AuthProvider>
                    <Routes>
                        <Route
                            path="/login"
                            element={<LoginPage />}
                        />
                        <Route
                            path="/change-password"
                            element={<ChangePassword />}
                        />
                        <Route element={<PrivateRoutes />}>
                            <Route path="/*" element={<App />} />
                        </Route>
                    </Routes>
                </AuthProvider>
            </BrowserRouter>
        </ChakraProvider>
    </React.StrictMode>,
);
