import {
    Accordion,
    AccordionButton,
    AccordionIcon,
    AccordionItem,
    AccordionPanel,
    Box,
    Center,
    StackDivider,
    useColorModeValue,
    VStack,
} from "@chakra-ui/react";
import dayjs from "dayjs";
import relativeTime from "dayjs/plugin/relativeTime";
import utc from "dayjs/plugin/utc";
import * as React from "react";
import { useState } from "react";
import { Route, Routes } from "react-router-dom";
import useWebSocket from "react-use-websocket";
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
    const longOrder = useLatestEvent<Order>(source, "order", intoOrder);

    const cfdsOrUndefined = useLatestEvent<Cfd[]>(source, "cfds", intoCfd);
    let cfds = cfdsOrUndefined ? cfdsOrUndefined! : [];
    const connectedToMakerOrUndefined = useLatestEvent<ConnectionStatus>(source, "maker_status");
    const connectedToMaker = connectedToMakerOrUndefined ? connectedToMakerOrUndefined : { online: false };

    // Global, currently always taken from long order because values are expected to be the same
    const minQuantity = parseOptionalNumber(longOrder?.min_quantity) || 0;
    const maxQuantity = parseOptionalNumber(longOrder?.max_quantity) || 0;
    const parcelSize = parseOptionalNumber(longOrder?.parcel_size) || 0;
    const leverage = longOrder?.leverage || 0;
    const openingFee = longOrder?.opening_fee || 0;
    const fundingRateAnnualized = longOrder?.funding_rate_annualized_percent || "0";
    const fundingRateHourly = longOrder
        ? Number.parseFloat(longOrder.funding_rate_hourly_percent).toFixed(5)
        : null;
    const liquidationPrice = parseOptionalNumber(longOrder?.liquidation_price);

    // Long specific
    const longOrderId = longOrder?.id;
    const longPrice = parseOptionalNumber(longOrder?.price);
    const longMarginPerParcel = longOrder?.margin_per_parcel;
    const longInitialFundingFeePerParcel = longOrder?.initial_funding_fee_per_parcel;

    // Short specific
    // TODO -> Add short

    dayjs.extend(relativeTime);
    dayjs.extend(utc);

    // TODO: Eventually this should be calculated with what the maker defines in the offer, for now we assume full hour
    const nextFullHour = dayjs().utc().minute(0).add(1, "hour");
    const nextFundingEvent = longOrder ? dayjs().to(nextFullHour) : null;

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

    return (
        <>
            <Disclaimer />
            <Nav
                walletInfo={walletInfo}
                connectedToMaker={connectedToMaker}
                fundingRate={fundingRateHourly}
                nextFundingEvent={nextFundingEvent}
                referencePrice={referencePrice}
            />
            <Box textAlign="center" padding={3} bg={useColorModeValue("gray.50", "gray.800")}>
                <Routes>
                    <Route
                        path="/wallet"
                        element={<>
                            <Center marginTop={20}>
                                <VStack>
                                    {connectionStatus}
                                    <Wallet walletInfo={walletInfo} />
                                </VStack>
                            </Center>
                        </>}
                    />
                    <Route
                        path="/"
                        element={<>
                            <Center marginTop={20}>
                                <VStack>
                                    {connectionStatus}
                                </VStack>
                            </Center>
                            <VStack divider={<StackDivider borderColor="gray.500" />} spacing={4}>
                                <Trade
                                    longOrderId={longOrderId}
                                    longPrice={longPrice}
                                    longMarginPerParcel={longMarginPerParcel}
                                    longInitialFundingFeePerParcel={longInitialFundingFeePerParcel}
                                    liquidationPrice={liquidationPrice}
                                    connectedToMaker={connectedToMaker}
                                    maxQuantity={maxQuantity}
                                    minQuantity={minQuantity}
                                    parcelSize={parcelSize}
                                    leverage={leverage}
                                    walletBalance={walletInfo ? walletInfo.balance : 0}
                                    openingFee={openingFee}
                                    fundingRateAnnualized={fundingRateAnnualized}
                                    fundingRateHourly={fundingRateHourly || "0"}
                                />
                                <History
                                    connectedToMaker={connectedToMaker}
                                    cfds={cfds.filter((cfd) => !isClosed(cfd))}
                                />

                                <Accordion allowToggle width={"100%"}>
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
                                                cfds={cfds.filter((cfd) => isClosed(cfd))}
                                                connectedToMaker={connectedToMaker}
                                            />
                                        </AccordionPanel>
                                    </AccordionItem>
                                </Accordion>
                            </VStack>
                        </>}
                    />
                </Routes>
            </Box>
            <Footer />
        </>
    );
};
