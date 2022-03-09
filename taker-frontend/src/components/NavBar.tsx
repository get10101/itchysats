import { HamburgerIcon, MoonIcon, SunIcon } from "@chakra-ui/icons";
import {
    Box,
    Button,
    Divider,
    Flex,
    Heading,
    HStack,
    Image,
    Link,
    Menu,
    MenuButton,
    MenuItem,
    MenuList,
    Skeleton,
    Stack,
    Text,
    Tooltip,
    useColorMode,
    useColorModeValue,
} from "@chakra-ui/react";
import * as React from "react";
import { useNavigate } from "react-router-dom";
import logoBlack from "../images/logo_nav_bar_black.svg";
import logoWhite from "../images/logo_nav_bar_white.svg";
import { ConnectionCloseReason, ConnectionStatus, WalletInfo } from "../types";
import DollarAmount from "./DollarAmount";

interface NavProps {
    walletInfo: WalletInfo | null;
    connectedToMaker: ConnectionStatus;
    nextFundingEvent: string | null;
    referencePrice: number | undefined;
}

function TextDivider() {
    return (
        <Divider orientation={"vertical"} borderColor={useColorModeValue("black", "white")} height={"20px"} />
    );
}

export default function Nav({ walletInfo, connectedToMaker, nextFundingEvent, referencePrice }: NavProps) {
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

    let connectionMessage = connectedToMaker.online
        ? { label: "Online", color: { light: "green.600", dark: "green.500" } }
        : { label: "Offline", color: { light: "red.600", dark: "red.500" } };

    if (connectedToMaker.connection_close_reason) {
        switch (connectedToMaker.connection_close_reason) {
            case ConnectionCloseReason.MAKER_VERSION_OUTDATED:
                connectionMessage.label = connectionMessage
                    .label + ": the maker is running an outdated version, please reach out to ItchySats!";
                break;
            case ConnectionCloseReason.TAKER_VERSION_OUTDATED:
                connectionMessage.label = connectionMessage.label
                    + ": you are running an incompatible version, please upgrade!";
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
                        <Text>{"Maker: "}</Text>
                        <Heading
                            size={"sm"}
                            color={useColorModeValue(connectionMessage.color.light, connectionMessage.color.dark)}
                        >
                            {connectionMessage.label}
                        </Heading>
                        <TextDivider />
                        <Text>{"Next funding event: "}</Text>
                        <Skeleton
                            isLoaded={nextFundingEvent != null}
                            height={"20px"}
                            display={"flex"}
                            alignItems={"center"}
                        >
                            <Tooltip
                                label={"The next time your CFDs will be extended and the funding fee will be collected based on the hourly rate."}
                                hasArrow
                            >
                                <HStack minWidth={"80px"}>
                                    <Heading size={"sm"}>{nextFundingEvent}</Heading>
                                </HStack>
                            </Tooltip>
                        </Skeleton>
                        <TextDivider />
                        <Text>{"Reference price: "}</Text>
                        <Skeleton
                            isLoaded={referencePrice !== undefined}
                            height={"20px"}
                            display={"flex"}
                            alignItems={"center"}
                        >
                            <Tooltip label={"The price the Oracle attests to, the BitMEX BXBT index price"} hasArrow>
                                <Link href={"https://outcome.observer/h00.ooo/x/BitMEX/BXBT"} target={"_blank"}>
                                    {/* The minWidth helps with not letting the elements in Nav jump because the width changes*/}
                                    <Heading size={"sm"} minWidth={"90px"}>
                                        <DollarAmount amount={referencePrice || 0} />
                                    </Heading>
                                </Link>
                            </Tooltip>
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
