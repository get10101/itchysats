import { HamburgerIcon, MoonIcon, SunIcon } from "@chakra-ui/icons";
import {
    Box,
    Button,
    Flex,
    Image,
    Menu,
    MenuButton,
    MenuItem,
    MenuList,
    Stack,
    useColorMode,
    useColorModeValue,
} from "@chakra-ui/react";
import * as React from "react";
import { useHistory } from "react-router-dom";
import logoBlack from "../images/logo_nav_bar_black.svg";
import logoWhite from "../images/logo_nav_bar_white.svg";
import { WalletInfo } from "./Types";
import { WalletNavBar } from "./Wallet";

interface NavProps {
    walletInfo: WalletInfo | null;
}

export default function Nav({ walletInfo }: NavProps) {
    const history = useHistory();

    const { toggleColorMode } = useColorMode();
    const navBarLog = useColorModeValue(
        <Image src={logoBlack} w="128px" />,
        <Image src={logoWhite} w="128px" />,
    );

    const toggleIcon = useColorModeValue(
        <MoonIcon />,
        <SunIcon />,
    );

    return (
        <>
            <Box bg={useColorModeValue("gray.100", "gray.900")} px={4}>
                <Flex h={16} alignItems={"center"} justifyContent={"space-between"}>
                    <Menu>
                        <MenuButton
                            as={Button}
                            variant={"link"}
                            cursor={"pointer"}
                            minW={0}
                        >
                            <HamburgerIcon w={"32px"} />
                        </MenuButton>
                        <MenuList alignItems={"center"}>
                            <MenuItem onClick={() => history.push("/trade")}>Home</MenuItem>
                            <MenuItem onClick={() => history.push("/wallet")}>Wallet</MenuItem>
                            <MenuItem>Settings</MenuItem>
                        </MenuList>
                    </Menu>

                    <WalletNavBar walletInfo={walletInfo} />

                    <Flex alignItems={"center"}>
                        <Stack direction={"row"} spacing={7}>
                            <Button onClick={toggleColorMode} bg={"transparent"}>
                                {toggleIcon}
                            </Button>
                            <Box>
                                <Button bg={"transparent"} onClick={() => history.push("/trade")}>
                                    {navBarLog}
                                </Button>
                            </Box>
                        </Stack>
                    </Flex>
                </Flex>
            </Box>
        </>
    );
}
