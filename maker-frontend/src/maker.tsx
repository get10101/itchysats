import { ChakraProvider } from "@chakra-ui/react";
import React from "react";
import ReactDOM from "react-dom";
import { BrowserRouter, Navigate, Outlet, Route, Routes } from "react-router-dom";
import { EventSourceProvider } from "react-sse-hooks";
import useAuth, { AuthProvider } from "./authentication/useAuth";
import ChangePassword from "./ChangePassword";
import "./index.css";
import LoginPage from "./LoginPage";
import App from "./MakerApp";
import theme from "./theme";

const PrivateRoutes = () => {
    const { authenticated, firstLogin } = useAuth();

    if (!authenticated) return <Navigate to="/login" />;
    if (firstLogin) return <Navigate to="/change-password" />;

    return <Outlet />;
};

ReactDOM.render(
    <React.StrictMode>
        <ChakraProvider theme={theme}>
            <EventSourceProvider>
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
                                <Route path="/" element={<App />} />
                            </Route>
                        </Routes>
                    </AuthProvider>
                </BrowserRouter>
            </EventSourceProvider>
        </ChakraProvider>
    </React.StrictMode>,
    document.getElementById("root"),
);
