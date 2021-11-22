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
import { useEffect, useState } from "react";
import { useAsync } from "react-async";
import { Route, Routes } from "react-router-dom";
import { useEventSource } from "react-sse-hooks";
import useWebSocket from "react-use-websocket";
import { useBackendMonitor } from "./components/BackendMonitor";
import createErrorToast from "./components/ErrorToast";
import Footer from "./components/Footer";
import History from "./components/History";
import { HttpError } from "./components/HttpError";
import Nav from "./components/NavBar";
import Trade from "./components/Trade";
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
    WithdrawRequest,
} from "./components/Types";
import { Wallet, WalletInfoBar } from "./components/Wallet";
import useLatestEvent from "./Hooks";

async function getMargin(payload: MarginRequestPayload): Promise<MarginResponse> {
    let res = await fetch(`/api/calculate/margin`, { method: "POST", body: JSON.stringify(payload) });

    if (!res.status.toString().startsWith("2")) {
        const resp = await res.json();
        throw new HttpError(resp);
    }

    return res.json();
}

async function postCfdOrderRequest(payload: CfdOrderRequestPayload) {
    let res = await fetch(`/api/cfd/order`, { method: "POST", body: JSON.stringify(payload) });
    if (!res.status.toString().startsWith("2")) {
        const resp = await res.json();
        throw new HttpError(resp);
    }
}

export async function postWithdraw(payload: WithdrawRequest) {
    let res = await fetch(`/api/withdraw`, { method: "POST", body: JSON.stringify(payload) });
    if (!res.status.toString().startsWith("2")) {
        const resp = await res.json();
        throw new HttpError(resp);
    }
    return res.text();
}

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

    let [quantity, setQuantity] = useState("0");
    let [margin, setMargin] = useState("0");
    let [userHasEdited, setUserHasEdited] = useState(false);

    const { price: askPrice, min_quantity, max_quantity, leverage, liquidation_price: liquidationPrice } = order || {};

    let effectiveQuantity = userHasEdited ? quantity : (min_quantity?.toString() || "0");

    let { run: calculateMargin } = useAsync({
        deferFn: async ([payload]: any[]) => {
            try {
                let res = await getMargin(payload as MarginRequestPayload);
                setMargin(res.margin.toString());
            } catch (e) {
                createErrorToast(toast, e);
            }
        },
    });

    let { run: makeNewOrderRequest, isLoading: isCreatingNewOrderRequest } = useAsync({
        deferFn: async ([payload]: any[]) => {
            try {
                await postCfdOrderRequest(payload as CfdOrderRequestPayload);
            } catch (e) {
                createErrorToast(toast, e);
            }
        },
    });

    useEffect(() => {
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
    }, // Eslint demands us to include `calculateMargin` in the list of dependencies.
     // We don't want that as we will end up in an endless loop. It is safe to ignore `calculateMargin` because
    // nothing in `calculateMargin` depends on outside values, i.e. is guaranteed to be stable.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [margin, effectiveQuantity, order]);

    const format = (val: any) => `$` + val;
    const parse = (val: any) => val.replace(/^\$/, "");

    return (
        <>
            <Nav walletInfo={walletInfo} />
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
                                        if (!order) {
                                            return;
                                        }
                                        let quantity = valueString ? Number.parseFloat(valueString) : 0;
                                        let payload: MarginRequestPayload = {
                                            leverage: order.leverage,
                                            price: order.price,
                                            quantity,
                                        };
                                        calculateMargin(payload);
                                    }}
                                    onLongSubmit={makeNewOrderRequest}
                                    isSubmitting={isCreatingNewOrderRequest}
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
