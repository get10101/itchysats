import { Button, Container, Flex, Grid, GridItem, HStack, Stack, Text, useToast, VStack } from "@chakra-ui/react";
import React, { useState } from "react";
import { useAsync } from "react-async";
import { useEventSource } from "react-sse-hooks";
import "./App.css";
import CfdTile from "./components/CfdTile";
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

    const cfds = useLatestEvent<Cfd[]>(source, "cfds");
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

    return (
        <Container maxWidth="120ch" marginTop="1rem">
            <Grid templateColumns="repeat(6, 1fr)" gap={4}>
                <GridItem colSpan={4}>
                    <Stack>
                        {cfds && cfds.map((cfd, index) =>
                            <CfdTile
                                key={"cfd_" + index}
                                index={index}
                                cfd={cfd}
                            />
                        )}
                    </Stack>
                </GridItem>
                <GridItem colStart={5} colSpan={2}>
                    <Wallet walletInfo={walletInfo} />
                    <VStack spacing={5} shadow={"md"} padding={5} align={"stretch"}>
                        <HStack>
                            {/*TODO: Do we need this? does it make sense to only display the price from the order?*/}
                            <Text align={"left"}>Current Price (Kraken):</Text>
                            <Text>tbd</Text>
                        </HStack>
                        <HStack>
                            <Text align={"left"}>Order Price:</Text>
                            <Text>{order?.price}</Text>
                        </HStack>
                        <HStack>
                            <Text>Quantity:</Text>
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
                            <Text>Margin in BTC:</Text>
                            <Text>{margin}</Text>
                        </HStack>
                        <Text>Leverage:</Text>
                        {/* TODO: consider button group */}
                        <Flex justifyContent={"space-between"}>
                            <Button disabled={true}>x1</Button>
                            <Button disabled={true}>x2</Button>
                            <Button colorScheme="blue" variant="solid">x{order?.leverage}</Button>
                        </Flex>
                        {<Button
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
                        </Button>}
                    </VStack>
                </GridItem>
            </Grid>
        </Container>
    );
}
