import { extendTheme } from "@chakra-ui/react";

const theme = extendTheme({
    textStyles: {
        smGray: {
            fontSize: "sm",
            color: "gray.500",
        },
        mdGray: {
            fontSize: "md",
            color: "gray.500",
        },
        lgGray: {
            fontSize: "lg",
            color: "gray.500",
        },
    },
    components: {
        Button: {
            variants: {
                "primary": {
                    color: "white",
                    bg: "blue.500",
                    _hover: {
                        bg: "blue.300",
                        _disabled: {
                            bg: "blue.300",
                        },
                    },
                    size: "md",
                },
                "secondary": {
                    bg: "gray.200",
                    size: "md",
                },
            },
        },
    },
});

export default theme;
