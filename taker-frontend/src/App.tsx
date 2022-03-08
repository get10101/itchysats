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
    VStack,
} from "@chakra-ui/react";
import dayjs from "dayjs";
import relativeTime from "dayjs/plugin/relativeTime";
import utc from "dayjs/plugin/utc";
import * as React from "react";
import { useState } from "react";
import { Navigate, Route, Routes, useLocation } from "react-router-dom";
import { Link as ReachLink } from "react-router-dom";
import useWebSocket from "react-use-websocket";
import { useLocalStorage } from "usehooks-ts";
import AlertBox from "./components/AlertBox";
import Disclaimer from "./components/Disclaimer";
import Footer from "./components/Footer";
import History from "./components/History";
import Nav from "./components/NavBar";
import Trade from "./components/Trade";
import { Wallet } from "./components/Wallet";
import { BXBTData, Cfd, ConnectionStatus, intoCfd, intoOrder, isClosed, Order, WalletInfo } from "./types";
import { useEventSource } from "./useEventSource";
import useLatestEvent from "./useLatestEvent";

export interface SomeOrder {
    id?: string;
    price?: number;
    marginPerParcel?: number;
    initialFundingFeePerParcel?: number;
}

export interface GlobalTradeParams {
    minQuantity: number;
    maxQuantity: number;
    parcelSize: number;
    leverage: number;
    openingFee: number;
    fundingRateAnnualized: string;
    fundingRateHourly: string;
    liquidationPrice?: number;
}

export const App = () => {
    let [referencePrice, setReferencePrice] = useState<number>();

    useWebSocket("wss://www.bitmex.com/realtime?subscribe=instrument:.BXBT", {
        shouldReconnect: () => true,
        onMessage: (message) => {
            const data: BXBTData[] = JSON.parse(message.data).data;
            if (data && data[0]?.markPrice) {
                setReferencePrice(data[0].markPrice);
            }
        },
    });

    const [source, isConnected] = useEventSource("/api/feed");
    const walletInfo = useLatestEvent<WalletInfo>(source, "wallet");

    const makerLong = useLatestEvent<Order>(source, "long_offer", intoOrder);
    const makerShort = useLatestEvent<Order>(source, "short_offer", intoOrder);

    const shortOrder = {
        id: makerLong?.id,
        initialFundingFeePerParcel: makerLong?.initial_funding_fee_per_parcel,
        marginPerParcel: makerLong?.margin_per_parcel,
        price: parseOptionalNumber(makerLong?.price),
    };

    const longOrder = {
        id: makerShort?.id,
        initialFundingFeePerParcel: makerShort?.initial_funding_fee_per_parcel,
        marginPerParcel: makerShort?.margin_per_parcel,
        price: parseOptionalNumber(makerShort?.price),
    };

    const globalTradeParams = extractGlobalTradeParams(makerLong, makerShort);

    function extractGlobalTradeParams(long: Order | null, short: Order | null) {
        const order = long ? long : short ? short : null;

        if (order) {
            return {
                minQuantity: parseOptionalNumber(order.min_quantity) || 0,
                maxQuantity: parseOptionalNumber(order.max_quantity) || 0,
                parcelSize: parseOptionalNumber(order.parcel_size) || 0,
                leverage: order.leverage || 0,
                openingFee: order.opening_fee || 0,
                fundingRateAnnualized: order.funding_rate_annualized_percent || "0",
                fundingRateHourly: order
                    ? Number.parseFloat(order.funding_rate_hourly_percent).toFixed(5)
                    : "0",
                liquidationPrice: parseOptionalNumber(order.liquidation_price),
            };
        }

        return {
            minQuantity: 0,
            maxQuantity: 0,
            parcelSize: 100,
            leverage: 2,
            openingFee: 0,
            fundingRateAnnualized: "0",
            fundingRateHourly: "0",
        };
    }

    const cfdsOrUndefined = useLatestEvent<Cfd[]>(source, "cfds", intoCfd);
    let cfds = cfdsOrUndefined ? cfdsOrUndefined! : [];
    const connectedToMakerOrUndefined = useLatestEvent<ConnectionStatus>(source, "maker_status");
    const connectedToMaker = connectedToMakerOrUndefined ? connectedToMakerOrUndefined : { online: false };

    dayjs.extend(relativeTime);
    dayjs.extend(utc);

    // TODO: Eventually this should be calculated with what the maker defines in the offer, for now we assume full hour
    const nextFullHour = dayjs().utc().minute(0).add(1, "hour");

    // TODO: this condition is a bit weird now
    const nextFundingEvent = longOrder || shortOrder ? dayjs().to(nextFullHour) : null;

    function parseOptionalNumber(val: string | undefined): number | undefined {
        if (!val) {
            return undefined;
        }

        return Number.parseFloat(val);
    }

    let connectionStatus;
    if (!isConnected) {
        connectionStatus = <AlertBox
            title={"Connection error!"}
            description={"Please ensure taker daemon is running and refresh page"}
        />;
    }

    const [hideDisclaimer, setHideDisclaimer] = useLocalStorage<boolean>("hideDisclaimer", false);

    const closedPositions = cfds.filter((cfd) => isClosed(cfd));

    const history = <>
        <History
            connectedToMaker={connectedToMaker}
            cfds={cfds.filter((cfd) => !isClosed(cfd))}
        />

        {closedPositions.length > 0
            && <Accordion allowToggle width={"100%"}>
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
                        />
                    </AccordionPanel>
                </AccordionItem>
            </Accordion>}
    </>;

    return (
        <>
            {!hideDisclaimer && <Disclaimer setHideDisclaimer={setHideDisclaimer} />}
            <Nav
                walletInfo={walletInfo}
                connectedToMaker={connectedToMaker}
                fundingRate={globalTradeParams?.fundingRateHourly}
                nextFundingEvent={nextFundingEvent}
                referencePrice={referencePrice}
            />
            <Box textAlign="center" padding={3} bg={useColorModeValue("gray.50", "gray.800")}>
                <Center marginTop={20}>
                    <VStack>
                        {connectionStatus}
                        <Routes>
                            <Route path="/">
                                <Route
                                    path="wallet"
                                    element={<>
                                        <Wallet walletInfo={walletInfo} />
                                    </>}
                                />
                                <Route
                                    path="long"
                                    element={<VStack>
                                        <NavigationButtons />
                                        <Trade
                                            order={longOrder}
                                            globalTradeParams={globalTradeParams}
                                            connectedToMaker={connectedToMaker}
                                            walletBalance={walletInfo ? walletInfo.balance : 0}
                                            isLong={true}
                                        />
                                        {history}
                                    </VStack>}
                                />
                                <Route
                                    path="short"
                                    element={<VStack>
                                        <NavigationButtons />
                                        <Trade
                                            order={shortOrder}
                                            globalTradeParams={globalTradeParams}
                                            connectedToMaker={connectedToMaker}
                                            walletBalance={walletInfo ? walletInfo.balance : 0}
                                            isLong={false}
                                        />
                                        {history}
                                    </VStack>}
                                />
                                <Route index element={<Navigate to="long" />} />
                            </Route>
                            <Route
                                path="/*"
                                element={<>
                                </>}
                            />
                        </Routes>
                    </VStack>
                </Center>
            </Box>
            <Footer />
        </>
    );
};

function NavigationButtons() {
    const location = useLocation();
    const isLongSelected = location.pathname.includes("long");
    const isShortSelected = !isLongSelected;

    const unSelectedButton = "transparent";
    const selectedButton = useColorModeValue("grey.400", "black.400");
    const buttonBorder = useColorModeValue("grey.400", "black.400");
    const buttonText = useColorModeValue("black", "white");

    return (<HStack>
        <Center>
            <ButtonGroup
                padding="3"
                spacing="6"
            >
                <Button
                    as={ReachLink}
                    to="/long"
                    color={isLongSelected ? selectedButton : unSelectedButton}
                    bg={isLongSelected ? selectedButton : unSelectedButton}
                    border={buttonBorder}
                    isActive={isLongSelected}
                    size="lg"
                    h={10}
                    w={"40"}
                >
                    <Text fontSize={"md"} color={buttonText}>Long BTC</Text>
                </Button>
                <Button
                    as={ReachLink}
                    to="/short"
                    color={isShortSelected ? selectedButton : unSelectedButton}
                    bg={isShortSelected ? selectedButton : unSelectedButton}
                    border={buttonBorder}
                    isActive={isShortSelected}
                    size="lg"
                    h={10}
                    w={"40"}
                >
                    <Text fontSize={"md"} color={buttonText}>Short BTC</Text>
                </Button>
            </ButtonGroup>
        </Center>
    </HStack>);
}
