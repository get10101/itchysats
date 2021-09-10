import { ChakraProvider } from "@chakra-ui/react";
import React from "react";
import ReactDOM from "react-dom";
import { BrowserRouter, Route, Routes } from "react-router-dom";
import Maker from "./Maker";
import theme from "./theme";

it("renders without crashing", () => {
    const div = document.createElement("div");
    ReactDOM.render(
        <React.StrictMode>
            <ChakraProvider theme={theme}>
                <BrowserRouter>
                    <Routes>
                        <Route path="/maker/*" element={<Maker />} />
                    </Routes>
                </BrowserRouter>
            </ChakraProvider>
        </React.StrictMode>,
        div,
    );
});
