import { Button, Container, Flex, Grid, GridItem, HStack, Stack, Text, useToast, VStack } from "@chakra-ui/react";
import React, { useState } from "react";
import { useAsync } from "react-async";
import { useEventSource } from "react-sse-hooks";
import CfdTile from "./components/CfdTile";
import CurrencyInputField from "./components/CurrencyInputField";
import useLatestEvent from "./components/Hooks";
import OrderTile from "./components/OrderTile";
import { Cfd, Order, WalletInfo } from "./components/Types";
import Wallet from "./components/Wallet";
import { CfdSellOrderPayload, postCfdSellOrderRequest } from "./MakerClient";

export default function App() {
    let source = useEventSource({ source: "/api/feed", options: { withCredentials: true } });

    const cfds = useLatestEvent<Cfd[]>(source, "cfds");
    const order = useLatestEvent<Order>(source, "order");

    console.log(cfds);

    const walletInfo = useLatestEvent<WalletInfo>(source, "wallet");

    const toast = useToast();
    let [minQuantity, setMinQuantity] = useState<string>("100");
    let [maxQuantity, setMaxQuantity] = useState<string>("1000");
    let [orderPrice, setOrderPrice] = useState<string>("10000");

    const format = (val: any) => `$` + val;
    const parse = (val: any) => val.replace(/^\$/, "");

    let { run: makeNewCfdSellOrder, isLoading: isCreatingNewCfdOrder } = useAsync({
        deferFn: async ([payload]: any[]) => {
            try {
                await postCfdSellOrderRequest(payload as CfdSellOrderPayload);
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
                            <Text align={"left"}>Current Price:</Text>
                            <Text>{49000}</Text>
                        </HStack>
                        <HStack>
                            <Text>Min Quantity:</Text>
                            <CurrencyInputField
                                onChange={(valueString: string) => setMinQuantity(parse(valueString))}
                                value={format(minQuantity)}
                            />
                        </HStack>
                        <HStack>
                            <Text>Min Quantity:</Text>
                            <CurrencyInputField
                                onChange={(valueString: string) => setMaxQuantity(parse(valueString))}
                                value={format(maxQuantity)}
                            />
                        </HStack>
                        <HStack>
                            <Text>Order Price:</Text>
                        </HStack>
                        <CurrencyInputField
                            onChange={(valueString: string) => setOrderPrice(parse(valueString))}
                            value={format(orderPrice)}
                        />
                        <Text>Leverage:</Text>
                        <Flex justifyContent={"space-between"}>
                            <Button disabled={true}>x1</Button>
                            <Button disabled={true}>x2</Button>
                            <Button colorScheme="blue" variant="solid">x{5}</Button>
                        </Flex>
                        <VStack>
                            <Button
                                disabled={isCreatingNewCfdOrder}
                                variant={"solid"}
                                colorScheme={"blue"}
                                onClick={() => {
                                    let payload: CfdSellOrderPayload = {
                                        price: Number.parseFloat(orderPrice),
                                        min_quantity: Number.parseFloat(minQuantity),
                                        max_quantity: Number.parseFloat(maxQuantity),
                                    };
                                    makeNewCfdSellOrder(payload);
                                }}
                            >
                                {order ? "Update Sell Order" : "Create Sell Order"}
                            </Button>
                            {order && <OrderTile order={order} />}
                        </VStack>
                    </VStack>
                </GridItem>
            </Grid>
        </Container>
    );
}
