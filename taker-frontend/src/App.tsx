import {
    Accordion,
    AccordionButton,
    AccordionIcon,
    AccordionItem,
    AccordionPanel,
    Box,
    Center,
    StackDivider,
    useToast,
    VStack,
} from "@chakra-ui/react";
import * as React from "react";
import { useState } from "react";
import { Route, Routes } from "react-router-dom";
import { useEventSource } from "react-sse-hooks";
import useWebSocket from "react-use-websocket";
import { useBackendMonitor } from "./components/BackendMonitor";
import Footer from "./components/Footer";
import History from "./components/History";
import Nav from "./components/NavBar";
import Trade from "./components/Trade";
import { Wallet, WalletInfoBar } from "./components/Wallet";
import {
    BXBTData,
    Cfd,
    CfdOrderRequestPayload,
    intoCfd,
    intoOrder,
    MarginRequestPayload,
    MarginResponse,
    Order,
    StateGroupKey,
    WalletInfo,
} from "./types";
import useDebouncedEffect from "./useDebouncedEffect";
import useLatestEvent from "./useLatestEvent";
import usePostRequest from "./usePostRequest";

export const App = () => {
    const toast = useToast();
    useBackendMonitor(toast, 5000, "Please start the taker again to reconnect..."); // 5s timeout

    const {
        lastMessage,
        readyState,
    } = useWebSocket("wss://www.bitmex.com/realtime?subscribe=instrument:.BXBT", {
        // Will attempt to reconnect on all close events, such as server shutting down
        shouldReconnect: () => true,
    });

    let referencePrice;
    if (readyState === 1 && lastMessage) {
        const data: BXBTData[] = JSON.parse(lastMessage.data).data;
        if (data && data[0]?.markPrice) {
            referencePrice = data[0].markPrice;
        }
    }

    let source = useEventSource({ source: "/api/feed" });
    const walletInfo = useLatestEvent<WalletInfo>(source, "wallet");
    const order = useLatestEvent<Order>(source, "order", intoOrder);
    const cfdsOrUndefined = useLatestEvent<Cfd[]>(source, "cfds", intoCfd);
    let cfds = cfdsOrUndefined ? cfdsOrUndefined! : [];
    cfds.sort((a, b) => a.order_id.localeCompare(b.order_id));
    const connectedToMakerOrUndefined = useLatestEvent<boolean>(source, "maker_status");
    const connectedToMaker = connectedToMakerOrUndefined ? connectedToMakerOrUndefined! : false;

    let [quantity, setQuantity] = useState("0");
    let [margin, setMargin] = useState(0);
    let [userHasEdited, setUserHasEdited] = useState(false);

    const { price: askPrice, min_quantity, max_quantity, leverage, liquidation_price: liquidationPrice } = order || {};

    let effectiveQuantity = userHasEdited ? quantity : (min_quantity?.toString() || "0");

    let [calculateMargin] = usePostRequest<MarginRequestPayload, MarginResponse>(
        "/api/calculate/margin",
        (response) => {
            setMargin(response.margin);
        },
    );
    let [makeNewOrderRequest, isCreatingNewOrderRequest] = usePostRequest<CfdOrderRequestPayload>("/api/cfd/order");

    useDebouncedEffect(
        () => {
            if (!order) {
                return;
            }
            let quantity = effectiveQuantity ? Number.parseFloat(effectiveQuantity) : 0;
            let payload: MarginRequestPayload = {
                leverage: order.leverage,
                price: order.price,
                quantity,
            };

            calculateMargin(payload);
        },
        [margin, effectiveQuantity, order],
        500,
    );

    const format = (val: any) => `$` + val;
    const parse = (val: any) => val.replace(/^\$/, "");

    return (
        <>
            <Nav walletInfo={walletInfo} connectedToMaker={connectedToMaker} />
            <Box textAlign="center" padding={3}>
                <Routes>
                    <Route path="/wallet" element={<Wallet walletInfo={walletInfo} />} />
                    <Route
                        path="/"
                        element={<>
                            <Center>
                                <WalletInfoBar walletInfo={walletInfo} />
                            </Center>
                            <VStack divider={<StackDivider borderColor="gray.500" />} spacing={4}>
                                <Trade
                                    connectedToMaker={connectedToMaker}
                                    orderId={order?.id}
                                    quantity={format(effectiveQuantity)}
                                    maxQuantity={max_quantity || 0}
                                    minQuantity={min_quantity || 0}
                                    referencePrice={referencePrice}
                                    askPrice={askPrice}
                                    margin={margin}
                                    leverage={leverage}
                                    liquidationPrice={liquidationPrice}
                                    onQuantityChange={(valueString: string) => {
                                        setUserHasEdited(true);
                                        setQuantity(parse(valueString));
                                    }}
                                    onLongSubmit={makeNewOrderRequest}
                                    isLongSubmitting={isCreatingNewOrderRequest}
                                />
                                <History
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
