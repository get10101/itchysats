import {
    Accordion,
    AccordionButton,
    AccordionIcon,
    AccordionItem,
    AccordionPanel,
    Box,
    Button,
    ButtonGroup,
    Center,
    HStack,
    Text,
    useColorModeValue,
    useDisclosure,
    VStack,
} from "@chakra-ui/react";
import * as React from "react";
import { Link as ReachLink, Outlet, useLocation, useNavigate, useParams } from "react-router-dom";
import { Symbol, VIEWPORT_WIDTH_PX } from "../App";
import { Cfd, ConnectionStatus, isClosed } from "../types";
import History from "./History";
import { SymbolSelector } from "./SymbolSelector";

interface TradePageLayoutProps {
    cfds: Cfd[];
    connectedToMaker: ConnectionStatus;
    showExtraInfo: boolean;
}

export function TradePageLayout({ cfds, connectedToMaker, showExtraInfo }: TradePageLayoutProps) {
    return (
        <VStack w={"100%"}>
            <NavigationButtons />
            <Outlet />
            <HistoryLayout cfds={cfds} connectedToMaker={connectedToMaker} showExtraInfo={showExtraInfo} />
        </VStack>
    );
}

interface HistoryLayoutProps {
    cfds: Cfd[];
    connectedToMaker: ConnectionStatus;
    showExtraInfo: boolean;
}

function HistoryLayout({ cfds, connectedToMaker, showExtraInfo }: HistoryLayoutProps) {
    const closedPositions = cfds.filter((cfd) => isClosed(cfd));

    return (
        <VStack padding={3} w={"100%"}>
            <History
                connectedToMaker={connectedToMaker}
                cfds={cfds.filter((cfd) => !isClosed(cfd))}
                showExtraInfo={showExtraInfo}
            />

            {closedPositions.length > 0
                && (
                    <Accordion allowToggle maxWidth={VIEWPORT_WIDTH_PX} width={"100%"}>
                        <AccordionItem>
                            <h2>
                                <AccordionButton>
                                    <AccordionIcon />
                                    <Box w={"100%"} textAlign="center">
                                        Show Closed Positions
                                    </Box>
                                    <AccordionIcon />
                                </AccordionButton>
                            </h2>
                            <AccordionPanel pb={4}>
                                <History
                                    cfds={closedPositions}
                                    connectedToMaker={connectedToMaker}
                                    showExtraInfo={showExtraInfo}
                                />
                            </AccordionPanel>
                        </AccordionItem>
                    </Accordion>
                )}
        </VStack>
    );
}

function NavigationButtons() {
    const location = useLocation();
    let { symbol: symbolString } = useParams();
    const navigate = useNavigate();
    let symbol = symbolString ? Symbol[symbolString as keyof typeof Symbol] : Symbol.btcusd;

    const isLongSelected = location.pathname.includes("long");
    const isShortSelected = !isLongSelected;

    const unSelectedButton = "transparent";
    const selectedButton = useColorModeValue("grey.400", "black.400");
    const buttonBorder = useColorModeValue("grey.400", "black.400");
    const buttonText = useColorModeValue("black", "white");

    let currentSymbol = symbolString ? Symbol[symbolString as keyof typeof Symbol] : Symbol.btcusd;
    const { onClose } = useDisclosure();
    const onSymbolChange = (symbol: string) => {
        onClose();
        navigate(`/trade/${symbol}/long`);
    };

    return (
        <HStack>
            <Center>
                <ButtonGroup
                    padding="3"
                    spacing={{ base: "3", md: "6" }}
                    id={"longShortButtonSwitch"}
                >
                    <Button
                        as={ReachLink}
                        to={"/trade/" + symbolString + "/long/"}
                        color={isLongSelected ? selectedButton : unSelectedButton}
                        bg={isLongSelected ? selectedButton : unSelectedButton}
                        border={buttonBorder}
                        borderWidth={isLongSelected ? "0px" : "2px"}
                        isActive={isLongSelected}
                        size="lg"
                        h={10}
                        w={{ base: "30", md: "40" }}
                    >
                        <Text fontSize={"md"} color={buttonText}>Long</Text>
                    </Button>
                    <Box minWidth={{ base: "150", md: "170" }}>
                        <SymbolSelector current={currentSymbol} onChange={onSymbolChange} />
                    </Box>
                    <Button
                        as={ReachLink}
                        to={"/trade/" + symbolString + "/short/"}
                        color={isShortSelected ? selectedButton : unSelectedButton}
                        bg={isShortSelected ? selectedButton : unSelectedButton}
                        border={buttonBorder}
                        borderWidth={isShortSelected ? "0px" : "2px"}
                        isActive={isShortSelected}
                        size="lg"
                        h={10}
                        w={{ base: "30", md: "40" }}
                    >
                        <Text fontSize={"md"} color={buttonText}>Short</Text>
                    </Button>
                </ButtonGroup>
            </Center>
        </HStack>
    );
}
