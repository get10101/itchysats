import { HamburgerIcon, LinkIcon, MoonIcon, SunIcon } from "@chakra-ui/icons";
import {
    Box,
    Button,
    Flex,
    Heading,
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
import { useNavigate } from "react-router-dom";
import logoBlack from "../images/logo_nav_bar_black.svg";
import logoWhite from "../images/logo_nav_bar_white.svg";
import { ConnectionCloseReason, ConnectionStatus, WalletInfo } from "../types";
import usePostRequest from "../usePostRequest";

interface NavProps {
    walletInfo: WalletInfo | null;
    connectedToMaker: ConnectionStatus;
}

export default function Nav({ walletInfo, connectedToMaker }: NavProps) {
    const navigate = useNavigate();

    const { toggleColorMode } = useColorMode();
    const navBarLog = useColorModeValue(
        <Image src={logoBlack} w="128px" />,
        <Image src={logoWhite} w="128px" />,
    );

    const toggleIcon = useColorModeValue(
        <MoonIcon />,
        <SunIcon />,
    );

    let connectionMessage = connectedToMaker.online ? "Online" : "Offline";
    if (connectedToMaker.connection_close_reason) {
        switch (connectedToMaker.connection_close_reason) {
            case ConnectionCloseReason.MAKER_VERSION_OUTDATED:
                connectionMessage = connectionMessage + ": the maker is running an outdated version";
                break;
            case ConnectionCloseReason.TAKER_VERSION_OUTDATED:
                connectionMessage = connectionMessage + " - you are running an incompatible version, please upgrade!";
                break;
        }
    }
    let [connect, isConnecting] = usePostRequest<{}, {}>("/api/connect");

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
                            <MenuItem onClick={() => navigate("/")}>Home</MenuItem>
                            <MenuItem onClick={() => navigate("/wallet")}>Wallet</MenuItem>
                        </MenuList>
                    </Menu>
                    <Heading size={"sm"}>{"Maker status: " + connectionMessage}</Heading>
                    <Flex alignItems={"center"}>
                        <Stack direction={"row"} spacing={7}>
                            <Button
                                onClick={() => connect({})}
                                disabled={connectedToMaker.online}
                                isLoading={isConnecting}
                                rightIcon={<LinkIcon />}
                            >
                                Reconnect
                            </Button>
                            <Button onClick={toggleColorMode} bg={"transparent"}>
                                {toggleIcon}
                            </Button>
                            <Box>
                                <Button bg={"transparent"} onClick={() => navigate("/")}>
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
