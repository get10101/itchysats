import {
    Accordion,
    AccordionButton,
    AccordionIcon,
    AccordionItem,
    AccordionPanel,
    Box,
    Center,
    StackDivider,
    VStack,
} from "@chakra-ui/react";
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
import { Wallet, WalletInfoBar } from "./components/Wallet";
import { BXBTData, Cfd, ConnectionStatus, intoCfd, intoOrder, Order, StateGroupKey, WalletInfo } from "./types";
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
    const order = useLatestEvent<Order>(source, "order", intoOrder);
    const cfdsOrUndefined = useLatestEvent<Cfd[]>(source, "cfds", intoCfd);
    let cfds = cfdsOrUndefined ? cfdsOrUndefined! : [];
    cfds.sort((a, b) => a.order_id.localeCompare(b.order_id));
    const connectedToMakerOrUndefined = useLatestEvent<ConnectionStatus>(source, "maker_status");
    const connectedToMaker = connectedToMakerOrUndefined ? connectedToMakerOrUndefined : { online: false };

    let [quantity, setQuantity] = useState("0");
    let [userHasEdited, setUserHasEdited] = useState(false);

    const {
        price: askPrice,
        min_quantity: minQuantity,
        max_quantity: maxQuantity,
        leverage,
        margin_per_parcel: marginPerParcel,
        parcel_size: parcelSize,
        liquidation_price: liquidationPrice,
    } = order || {};

    let effectiveQuantity = userHasEdited ? quantity : (minQuantity?.toString() || "0");

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
            <Nav walletInfo={walletInfo} connectedToMaker={connectedToMaker} />
            <Box textAlign="center" padding={3}>
                <Routes>
                    <Route
                        path="/wallet"
                        element={<>
                            <Center>
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
                            <Center>
                                <VStack>
                                    {connectionStatus}
                                    <WalletInfoBar walletInfo={walletInfo} />
                                </VStack>
                            </Center>
                            <VStack divider={<StackDivider borderColor="gray.500" />} spacing={4}>
                                <Trade
                                    connectedToMaker={connectedToMaker}
                                    orderId={order?.id}
                                    quantity={effectiveQuantity}
                                    maxQuantity={parseOptionalNumber(maxQuantity) || 0}
                                    minQuantity={parseOptionalNumber(minQuantity) || 0}
                                    referencePrice={referencePrice}
                                    askPrice={parseOptionalNumber(askPrice)}
                                    parcelSize={parseOptionalNumber(parcelSize) || 0}
                                    marginPerParcel={marginPerParcel || 0}
                                    leverage={leverage}
                                    liquidationPrice={parseOptionalNumber(liquidationPrice)}
                                    walletBalance={walletInfo ? walletInfo.balance : 0}
                                    onQuantityChange={(valueString: string) => {
                                        setUserHasEdited(true);
                                        setQuantity(valueString);
                                    }}
                                />
                                <History
                                    connectedToMaker={connectedToMaker}
                                    cfds={cfds.filter((cfd) => cfd.state.getGroup() !== StateGroupKey.CLOSED)}
                                    title={"Open Positions"}
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
                                                cfds={cfds.filter((cfd) =>
                                                    cfd.state.getGroup() === StateGroupKey.CLOSED
                                                )}
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
