import { ChakraProvider } from "@chakra-ui/react";
import React from "react";
import ReactDOM from "react-dom";
import { BrowserRouter } from "react-router-dom";
import { App } from "./App";
import "./index.css";
import theme from "./theme";

ReactDOM.render(
    <React.StrictMode>
        <ChakraProvider theme={theme}>
            <BrowserRouter>
                <App />
            </BrowserRouter>
        </ChakraProvider>
    </React.StrictMode>,
    document.getElementById("root"),
);
