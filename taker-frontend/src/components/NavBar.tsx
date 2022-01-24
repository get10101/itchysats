import { HamburgerIcon, MoonIcon, SunIcon } from "@chakra-ui/icons";
import {
    Box,
    Button,
    Divider,
    Flex,
    Heading,
    HStack,
    Image,
    Menu,
    MenuButton,
    MenuItem,
    MenuList,
    Skeleton,
    Stack,
    Text,
    useColorMode,
    useColorModeValue,
} from "@chakra-ui/react";
import * as React from "react";
import { useNavigate } from "react-router-dom";
import logoBlack from "../images/logo_nav_bar_black.svg";
import logoWhite from "../images/logo_nav_bar_white.svg";
import { ConnectionCloseReason, ConnectionStatus, WalletInfo } from "../types";

interface NavProps {
    walletInfo: WalletInfo | null;
    connectedToMaker: ConnectionStatus;
    fundingRate: string | null;
    nextFundingEvent: string | null;
}

export default function Nav({ walletInfo, connectedToMaker, fundingRate, nextFundingEvent }: NavProps) {
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
                connectionMessage = connectionMessage
                    + ": the maker is running an outdated version, please reach out to ItchySats!";
                break;
            case ConnectionCloseReason.TAKER_VERSION_OUTDATED:
                connectionMessage = connectionMessage + ": you are running an incompatible version, please upgrade!";
                break;
        }
    }

    return (
        <>
            <Box bg={useColorModeValue("gray.100", "gray.900")} px={4} position={"fixed"} width={"100%"} zIndex={"100"}>
                <Flex h={16} alignItems={"center"} justifyContent={"space-between"}>
                    <Flex justifyContent={"flex-left"} width={"180px"}>
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
                    </Flex>
                    <HStack>
                        <Text>{"Maker status: "}</Text>
                        <Heading size={"sm"}>{connectionMessage}</Heading>
                        <Divider orientation={"vertical"} borderColor={"black"} height={"20px"} />
                        <Text>{"Next funding event: "}</Text>
                        <Skeleton
                            isLoaded={nextFundingEvent != null}
                            height={"20px"}
                            minWidth={"50px"}
                            display={"flex"}
                            alignItems={"center"}
                        >
                            <Heading size={"sm"}>{nextFundingEvent}</Heading>
                        </Skeleton>
                        <Divider orientation={"vertical"} borderColor={"black"} height={"20px"} />
                        <Text>{"Current funding rate: "}</Text>
                        <Skeleton
                            isLoaded={fundingRate != null}
                            height={"20px"}
                            minWidth={"50px"}
                            display={"flex"}
                            alignItems={"center"}
                        >
                            <Heading size={"sm"}>{fundingRate}%</Heading>
                        </Skeleton>
                    </HStack>
                    <Flex alignItems={"center"}>
                        <Stack direction={"row"} spacing={7}>
                            <Button onClick={toggleColorMode} variant={"unstyled"}>
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
