import { HamburgerIcon, MoonIcon, SunIcon } from "@chakra-ui/icons";
import {
    AlertDialog,
    AlertDialogBody,
    AlertDialogContent,
    AlertDialogFooter,
    AlertDialogHeader,
    AlertDialogOverlay,
    Icon,
    IconButton,
    Tooltip,
} from "@chakra-ui/react";
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
import { FocusableElement } from "@chakra-ui/utils";
import * as React from "react";
import { RiRestartLine } from "react-icons/ri";
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

    let [forceStop, _] = usePostRequest<{}, {}>("/api/force-stop");

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
                            <ForceStopBackendButton onClick={() => forceStop({})} />
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

interface ForceStopBackendButtonProps {
    onClick: () => void;
}

function ForceStopBackendButton({ onClick }: ForceStopBackendButtonProps) {
    const [isOpen, setIsOpen] = React.useState(false);
    const onClose = () => setIsOpen(false);
    const cancelRef = React.useRef();

    return <>
        <Tooltip label="Force-stop the application">
            <IconButton
                aria-label="Force-stop"
                onClick={() => {
                    setIsOpen(true);
                }}
                icon={<Icon as={RiRestartLine} />}
            />
        </Tooltip>

        <AlertDialog
            isOpen={isOpen}
            // @ts-ignore: I don't know what you want ...
            leastDestructiveRef={cancelRef}
            onClose={onClose}
        >
            <AlertDialogOverlay>
                <AlertDialogContent>
                    <AlertDialogHeader fontSize="lg" fontWeight="bold">
                        Force-stop the application
                    </AlertDialogHeader>

                    <AlertDialogBody>
                        Have you tried turning it off and on again? Force-stopping the application will trigger a
                        restart which might help fix certain problems. Please refresh the page manually afterwards.
                    </AlertDialogBody>

                    <AlertDialogFooter>
                        {/*@ts-ignore: I don't know what you want ...*/}
                        <Button ref={cancelRef} onClick={onClose}>
                            Cancel
                        </Button>
                        <Button
                            colorScheme="red"
                            onClick={() => {
                                onClick();
                                onClose();
                            }}
                            ml={3}
                        >
                            Force-stop
                        </Button>
                    </AlertDialogFooter>
                </AlertDialogContent>
            </AlertDialogOverlay>
        </AlertDialog>
    </>;
}
