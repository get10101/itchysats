import { extendTheme, ThemeConfig } from "@chakra-ui/react";

let themeConfig: ThemeConfig = {
    initialColorMode: "dark",
    useSystemColorMode: false,
};

const theme = extendTheme({ themeConfig });

export default theme;
