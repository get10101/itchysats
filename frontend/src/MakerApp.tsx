import {
    Box,
    Button,
    Container,
    Divider,
    Grid,
    GridItem,
    HStack,
    Switch,
    Tab,
    TabList,
    TabPanel,
    TabPanels,
    Tabs,
    Text,
    useToast,
    VStack,
} from "@chakra-ui/react";
import React, { useEffect, useState } from "react";
import { useAsync } from "react-async";
import { useEventSource } from "react-sse-hooks";
import { CfdTable } from "./components/cfdtables/CfdTable";
import CurrencyInputField from "./components/CurrencyInputField";
import CurrentPrice from "./components/CurrentPrice";
import useLatestEvent from "./components/Hooks";
import OrderTile from "./components/OrderTile";
import { Cfd, intoCfd, intoOrder, Order, PriceInfo, StateGroupKey, WalletInfo } from "./components/Types";
import Wallet from "./components/Wallet";
import { CfdSellOrderPayload, postCfdSellOrderRequest } from "./MakerClient";

const SPREAD = 1.01;

export default function App() {
    let source = useEventSource({ source: "/api/feed", options: { withCredentials: true } });

    const cfdsOrUndefined = useLatestEvent<Cfd[]>(source, "cfds", intoCfd);
    let cfds = cfdsOrUndefined ? cfdsOrUndefined! : [];
    const order = useLatestEvent<Order>(source, "order", intoOrder);
    const walletInfo = useLatestEvent<WalletInfo>(source, "wallet");
    const priceInfo = useLatestEvent<PriceInfo>(source, "quote");

    const toast = useToast();
    let [minQuantity, setMinQuantity] = useState<string>("10");
    let [maxQuantity, setMaxQuantity] = useState<string>("100");
    let [orderPrice, setOrderPrice] = useState<string>("0");
    let [autoRefresh, setAutoRefresh] = useState(true);

    useEffect(() => {
        if (autoRefresh && priceInfo) {
            setOrderPrice((priceInfo.ask * SPREAD).toFixed(2).toString());
        }
    }, [priceInfo, autoRefresh]);

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

    const acceptOrRejectOrder = cfds.filter((value) => value.state.getGroup() === StateGroupKey.ACCEPT_OR_REJECT_ORDER);
    const acceptOrRejectSettlement = cfds.filter((value) =>
        value.state.getGroup() === StateGroupKey.ACCEPT_OR_REJECT_SETTLEMENT
    );
    const acceptOrRejectRollOvers = cfds.filter((value) =>
        value.state.getGroup() === StateGroupKey.ACCEPT_OR_REJECT_ROLL_OVER
    );
    const opening = cfds.filter((value) => value.state.getGroup() === StateGroupKey.OPENING);
    const open = cfds.filter((value) => value.state.getGroup() === StateGroupKey.OPEN);
    const closed = cfds.filter((value) => value.state.getGroup() === StateGroupKey.CLOSED);

    return (
        <Container maxWidth="120ch" marginTop="1rem">
            <HStack spacing={5}>
                <VStack>
                    <Wallet walletInfo={walletInfo} />

                    <Grid
                        gridTemplateColumns="max-content auto"
                        padding={5}
                        rowGap={5}
                        columnGap={5}
                        shadow="md"
                        width="100%"
                        alignItems="center"
                    >
                        <Text align={"left"}>Reference Price:</Text>
                        <CurrentPrice priceInfo={priceInfo} />

                        <Text>Min Quantity:</Text>
                        <CurrencyInputField
                            onChange={(valueString: string) => setMinQuantity(parse(valueString))}
                            value={format(minQuantity)}
                        />

                        <Text>Min Quantity:</Text>
                        <CurrencyInputField
                            onChange={(valueString: string) => setMaxQuantity(parse(valueString))}
                            value={format(maxQuantity)}
                        />

                        <Text>Order Price:</Text>
                        <HStack>
                            <CurrencyInputField
                                onChange={(valueString: string) => {
                                    setOrderPrice(parse(valueString));
                                    setAutoRefresh(false);
                                }}
                                value={format(orderPrice)}
                            />
                            <HStack>
                                <Switch
                                    id="auto-refresh"
                                    isChecked={autoRefresh}
                                    onChange={() => setAutoRefresh(!autoRefresh)}
                                />
                                <Text>Auto-refresh</Text>
                            </HStack>
                        </HStack>

                        <Text>Leverage:</Text>
                        <HStack spacing={5}>
                            <Button disabled={true}>x1</Button>
                            <Button colorScheme="blue" variant="solid">x{2}</Button>
                            <Button disabled={true}>x5</Button>
                        </HStack>

                        <GridItem colSpan={2}>
                            <Divider colSpan={2} />
                        </GridItem>

                        <GridItem colSpan={2} textAlign="center">
                            <Button
                                disabled={isCreatingNewCfdOrder || orderPrice === "0"}
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
                        </GridItem>
                    </Grid>
                </VStack>
                {order && <OrderTile order={order} />}
                <Box width="40%" />
            </HStack>

            <Tabs marginTop={5}>
                <TabList>
                    <Tab>Open [{open.length}]</Tab>
                    <Tab>Accept / Reject Order [{acceptOrRejectOrder.length}]</Tab>
                    <Tab>Accept / Reject Settlement [{acceptOrRejectSettlement.length}]</Tab>
                    <Tab>Accept / Reject Roll Overs [{acceptOrRejectRollOvers.length}]</Tab>
                    <Tab>Opening [{opening.length}]</Tab>
                    <Tab>Closed [{closed.length}]</Tab>
                </TabList>

                <TabPanels>
                    <TabPanel>
                        <CfdTable data={open} />
                    </TabPanel>
                    <TabPanel>
                        <CfdTable data={acceptOrRejectOrder} />
                    </TabPanel>
                    <TabPanel>
                        <CfdTable data={acceptOrRejectSettlement} />
                    </TabPanel>
                    <TabPanel>
                        <CfdTable data={acceptOrRejectRollOvers} />
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

function format(val: any) {
    return `$` + val;
}

function parse(val: any) {
    return val.replace(/^\$/, "");
}
