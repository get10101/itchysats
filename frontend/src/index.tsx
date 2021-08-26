import { ChakraProvider } from "@chakra-ui/react";
import React from "react";
import ReactDOM from "react-dom";
import { BrowserRouter, Route, Routes } from "react-router-dom";
import { EventSourceProvider } from "react-sse-hooks";
import "./index.css";
import Maker from "./Maker";
import Taker from "./Taker";
import theme from "./theme";

ReactDOM.render(
    <React.StrictMode>
        <ChakraProvider theme={theme}>
            <EventSourceProvider>
                <BrowserRouter>
                    <Routes>
                        <Route path="/maker/*" element={<Maker />} />
                        <Route path="/taker/*" element={<Taker />} />
                    </Routes>
                </BrowserRouter>
            </EventSourceProvider>
        </ChakraProvider>
    </React.StrictMode>,
    document.getElementById("root"),
);
