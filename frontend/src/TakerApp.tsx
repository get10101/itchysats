import {
    Box,
    Button,
    Container,
    Divider,
    HStack,
    Tab,
    TabList,
    TabPanel,
    TabPanels,
    Tabs,
    Text,
    useToast,
    VStack,
} from "@chakra-ui/react";
import React, { useState } from "react";
import { useAsync } from "react-async";
import { useEventSource } from "react-sse-hooks";
import { CfdTable } from "./components/cfdtables/CfdTable";
import CurrencyInputField from "./components/CurrencyInputField";
import useLatestEvent from "./components/Hooks";
import { Cfd, Order, WalletInfo } from "./components/Types";
import Wallet from "./components/Wallet";

interface CfdOrderRequestPayload {
    order_id: string;
    quantity: number;
}

interface MarginRequestPayload {
    price: number;
    quantity: number;
    leverage: number;
}

interface MarginResponse {
    margin: number;
}

async function postCfdOrderRequest(payload: CfdOrderRequestPayload) {
    let res = await fetch(`/api/cfd`, { method: "POST", body: JSON.stringify(payload) });

    if (!res.status.toString().startsWith("2")) {
        throw new Error("failed to create new CFD order request: " + res.status + ", " + res.statusText);
    }
}

async function getMargin(payload: MarginRequestPayload): Promise<MarginResponse> {
    let res = await fetch(`/api/calculate/margin`, { method: "POST", body: JSON.stringify(payload) });

    if (!res.status.toString().startsWith("2")) {
        throw new Error("failed to create new CFD order request: " + res.status + ", " + res.statusText);
    }

    return res.json();
}

export default function App() {
    let source = useEventSource({ source: "/api/feed" });

    const cfdsOrUndefined = useLatestEvent<Cfd[]>(source, "cfds");
    let cfds = cfdsOrUndefined ? cfdsOrUndefined! : [];
    const order = useLatestEvent<Order>(source, "order");
    const walletInfo = useLatestEvent<WalletInfo>(source, "wallet");

    const toast = useToast();
    let [quantity, setQuantity] = useState("0");
    let [margin, setMargin] = useState("0");

    let { run: calculateMargin } = useAsync({
        deferFn: async ([payload]: any[]) => {
            try {
                let res = await getMargin(payload as MarginRequestPayload);
                setMargin(res.margin.toString());
            } catch (e) {
                const description = typeof e === "string" ? e : JSON.stringify(e);

                toast({
                    title: "Error",
                    description,
                    status: "error",
                    duration: 9000,
                    isClosable: true,
                });
            }
        },
    });

    const format = (val: any) => `$` + val;
    const parse = (val: any) => val.replace(/^\$/, "");

    let { run: makeNewOrderRequest, isLoading: isCreatingNewOrderRequest } = useAsync({
        deferFn: async ([payload]: any[]) => {
            try {
                await postCfdOrderRequest(payload as CfdOrderRequestPayload);
            } catch (e) {
                const description = typeof e === "string" ? e : JSON.stringify(e);

                toast({
                    title: "Error",
                    description,
                    status: "error",
                    duration: 9000,
                    isClosable: true,
                });
            }
        },
    });

    const open = cfds.filter((value) => value.state.meta_state === "open");
    const opening = cfds.filter((value) => value.state.meta_state === "opening");
    const closed = cfds.filter((value) => value.state.meta_state === "closed");

    const labelWidth = 120;

    return (
        <Container maxWidth="120ch" marginTop="1rem">
            <HStack spacing={5}>
                <VStack>
                    <Wallet walletInfo={walletInfo} />
                    <VStack shadow={"md"} padding={5} align="stretch" spacing={5} width="100%">
                        <HStack>
                            <Text align={"left"} width={labelWidth}>Order Price:</Text>
                            <Text>{order?.price}</Text>
                        </HStack>
                        <HStack>
                            <Text width={labelWidth}>Quantity:</Text>
                            <CurrencyInputField
                                onChange={(valueString: string) => {
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
                                value={format(quantity)}
                            />
                        </HStack>
                        <HStack>
                            <Text width={labelWidth}>Margin in BTC:</Text>
                            <Text>{margin}</Text>
                        </HStack>
                        <HStack>
                            <Text width={labelWidth}>Leverage:</Text>
                            <HStack spacing={5}>
                                <Button disabled={true}>x1</Button>
                                <Button disabled={true}>x2</Button>
                                <Button colorScheme="blue" variant="solid">x{order?.leverage}</Button>
                            </HStack>
                        </HStack>
                        <Divider />
                        <Button
                            disabled={isCreatingNewOrderRequest || !order}
                            variant={"solid"}
                            colorScheme={"blue"}
                            onClick={() => {
                                let payload: CfdOrderRequestPayload = {
                                    order_id: order!.id,
                                    quantity: Number.parseFloat(quantity),
                                };
                                makeNewOrderRequest(payload);
                            }}
                        >
                            BUY
                        </Button>
                    </VStack>
                </VStack>
                <Box width="100%" />
            </HStack>
            <Tabs marginTop={5}>
                <TabList>
                    <Tab>Open [{open.length}]</Tab>
                    <Tab>Opening [{opening.length}]</Tab>
                    <Tab>Closed [{closed.length}]</Tab>
                </TabList>
                <TabPanels>
                    <TabPanel>
                        <CfdTable data={open} />
                    </TabPanel>
                    <TabPanel>
                        <CfdTable data={opening} />
                    </TabPanel>
                    <TabPanel>
                        <CfdTable data={closed} />
                    </TabPanel>
                </TabPanels>
            </Tabs>
        </Container>
    );
}
