import { extendTheme, ThemeConfig } from "@chakra-ui/react";

let themeConfig: ThemeConfig = {
    initialColorMode: "dark",
    useSystemColorMode: false,
};

const theme = extendTheme({
    themeConfig,
    components: {
        NumberInput: {
            variants: {
                outline: (props: any) => ({
                    field: {
                        background: props.colorMode === "dark" ? "gray.600" : "white",
                    },
                }),
            },
        },
        Button: {
            variants: {
                solid: (props: any) => ({
                    textColor: props.colorMode === "dark" ? "gray.600" : "white",
                }),
            },
        },
    },
});

export default theme;
