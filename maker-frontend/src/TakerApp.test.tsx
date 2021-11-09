import { ChakraProvider } from "@chakra-ui/react";
import React from "react";
import ReactDOM from "react-dom";
import Taker from "./TakerApp";
import theme from "./theme";

it("renders without crashing", () => {
    const div = document.createElement("div");
    ReactDOM.render(
        <React.StrictMode>
            <ChakraProvider theme={theme}>
                <Taker />
            </ChakraProvider>
        </React.StrictMode>,
        div,
    );
});
