import { ChakraProvider } from "@chakra-ui/react";
import React from "react";
import ReactDOM from "react-dom";
import Maker from "./MakerApp";
import theme from "./theme";

it("renders without crashing", () => {
    const div = document.createElement("div");
    ReactDOM.render(
        <React.StrictMode>
            <ChakraProvider theme={theme}>
                <Maker />
            </ChakraProvider>
        </React.StrictMode>,
        div,
    );
});
