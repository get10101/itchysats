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
import { CfdTableMaker } from "./components/cfdtables/CfdTableMaker";
import CurrencyInputField from "./components/CurrencyInputField";
import useLatestEvent from "./components/Hooks";
import OrderTile from "./components/OrderTile";
import { Cfd, Order, WalletInfo } from "./components/Types";
import Wallet from "./components/Wallet";
import { CfdSellOrderPayload, postCfdSellOrderRequest } from "./MakerClient";

export default function App() {
    let source = useEventSource({ source: "/api/feed", options: { withCredentials: true } });

    const cfdsOrUndefined = useLatestEvent<Cfd[]>(source, "cfds");
    let cfds = cfdsOrUndefined ? cfdsOrUndefined! : [];
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

    const acceptOrReject = cfds.filter((value) => value.state.meta_state === "acceptreject");
    const opening = cfds.filter((value) => value.state.meta_state === "opening");
    const open = cfds.filter((value) => value.state.meta_state === "open");
    const closed = cfds.filter((value) => value.state.meta_state === "closed");

    const labelWidth = 110;

    return (
        <Container maxWidth="120ch" marginTop="1rem">
            <HStack spacing={5}>
                <VStack>
                    <Wallet walletInfo={walletInfo} />
                    <VStack spacing={5} shadow={"md"} padding={5} width="100%" align={"stretch"}>
                        <HStack>
                            <Text width={labelWidth} align={"left"}>Current Price:</Text>
                            <Text>{49000}</Text>
                        </HStack>
                        <HStack>
                            <Text width={labelWidth}>Min Quantity:</Text>
                            <CurrencyInputField
                                onChange={(valueString: string) => setMinQuantity(parse(valueString))}
                                value={format(minQuantity)}
                            />
                        </HStack>
                        <HStack>
                            <Text width={labelWidth}>Min Quantity:</Text>
                            <CurrencyInputField
                                onChange={(valueString: string) => setMaxQuantity(parse(valueString))}
                                value={format(maxQuantity)}
                            />
                        </HStack>
                        <HStack>
                            <Text width={labelWidth}>Order Price:</Text>
                            <CurrencyInputField
                                onChange={(valueString: string) => setOrderPrice(parse(valueString))}
                                value={format(orderPrice)}
                            />
                        </HStack>
                        <HStack>
                            <Text width={labelWidth}>Leverage:</Text>
                            <HStack spacing={5}>
                                <Button disabled={true}>x1</Button>
                                <Button disabled={true}>x2</Button>
                                <Button colorScheme="blue" variant="solid">x{5}</Button>
                            </HStack>
                        </HStack>
                        <Divider />
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
                    </VStack>
                </VStack>
                {order && <OrderTile order={order} />}
                <Box width="40%" />
            </HStack>

            <Tabs marginTop={5}>
                <TabList>
                    <Tab>Open [{open.length}]</Tab>
                    <Tab>Accept/Reject [{acceptOrReject.length}]</Tab>
                    <Tab>Opening [{opening.length}]</Tab>
                    <Tab>Closed [{closed.length}]</Tab>
                </TabList>

                <TabPanels>
                    <TabPanel>
                        <CfdTable data={open} />
                    </TabPanel>
                    <TabPanel>
                        <CfdTableMaker data={acceptOrReject} />
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
