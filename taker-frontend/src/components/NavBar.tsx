import { HamburgerIcon, MoonIcon, SunIcon } from "@chakra-ui/icons";
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
import { ConnectionStatus, OfflineReason, WalletInfo } from "../types";

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
    if (connectedToMaker.offline_reason) {
        switch (connectedToMaker.offline_reason) {
            case OfflineReason.MAKER_VERSION_OUTDATED:
                connectionMessage = connectionMessage + ": the maker is running an outdated version";
                break;
            case OfflineReason.TAKER_VERSION_OUTDATED:
                connectionMessage = connectionMessage + ": you are running an incompatible version, please upgrade!";
                break;
            case OfflineReason.HEARTBEAT_TIMEOUT:
                connectionMessage = connectionMessage + ": lost connection";
                break;
            case OfflineReason.UNABLE_TO_CONNECT:
                connectionMessage = connectionMessage + ": maker unreachable";
                break;
            case OfflineReason.NOT_YET_CONNECTED:
                connectionMessage = connectionMessage + ": connecting ...";
                break;
        }
    }

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
