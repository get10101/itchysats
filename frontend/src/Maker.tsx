import {
    Box,
    Button,
    Center,
    Divider,
    Flex,
    Grid,
    GridItem,
    HStack,
    SimpleGrid,
    StackDivider,
    Text,
    useToast,
    VStack,
} from "@chakra-ui/react";
import React, { useState } from "react";
import { useAsync } from "react-async";
import { useEventSource } from "react-sse-hooks";
import "./App.css";
import CfdTile from "./components/CfdTile";
import CurrencyInputField from "./components/CurrencyInputField";
import useLatestEvent from "./components/Hooks";
import OrderTile from "./components/OrderTile";
import { Cfd, Order, WalletInfo } from "./components/Types";
import Wallet from "./components/Wallet";

interface CfdSellOrderPayload {
    price: number;
    min_quantity: number;
    max_quantity: number;
}

async function postCfdSellOrderRequest(payload: CfdSellOrderPayload) {
    let res = await fetch(`/api/order/sell`, { method: "POST", body: JSON.stringify(payload), credentials: "include" });

    if (!res.status.toString().startsWith("2")) {
        console.log("Status: " + res.status + ", " + res.statusText);
        throw new Error("failed to publish new order");
    }
}

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
        <Center>
            <Grid templateColumns="repeat(6, 1fr)" gap={4}>
                <GridItem colSpan={4}>
                    <VStack
                        spacing={5}
                        shadow={"md"}
                        padding={5}
                        width={"100%"}
                        divider={<StackDivider borderColor="gray.200" />}
                    >
                        <Box width={"100%"} overflow={"scroll"}>
                            <SimpleGrid columns={2} spacing={10}>
                                {cfds && cfds.map((cfd, index) =>
                                    <CfdTile
                                        key={"cfd_" + index}
                                        index={index}
                                        cfd={cfd}
                                    />
                                )}
                            </SimpleGrid>
                        </Box>
                    </VStack>
                </GridItem>
                <GridItem colStart={5} colSpan={2}>
                    <Wallet walletInfo={walletInfo} />
                </GridItem>
                <GridItem colStart={5} colSpan={2}>
                    <VStack spacing={5} shadow={"md"} padding={5} align={"stretch"} height={"100%"}>
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
                            <Center><Text>Maker UI</Text></Center>
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
                            <Divider />
                            <Box width={"100%"} overflow={"scroll"}>
                                <Box>
                                    {order
                                        && <OrderTile
                                            order={order}
                                        />}
                                </Box>
                            </Box>
                        </VStack>
                    </VStack>
                </GridItem>
            </Grid>
        </Center>
    );
}
