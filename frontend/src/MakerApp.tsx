import { RepeatIcon } from "@chakra-ui/icons";
import {
    Box,
    Button,
    Container,
    Divider,
    Grid,
    GridItem,
    HStack,
    IconButton,
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
import CurrentPrice from "./components/CurrentPrice";
import useLatestEvent from "./components/Hooks";
import OrderTile from "./components/OrderTile";
import { Cfd, intoCfd, intoOrder, Order, PriceInfo, StateGroupKey, WalletInfo } from "./components/Types";
import Wallet from "./components/Wallet";
import { CfdSellOrderPayload, postCfdSellOrderRequest } from "./MakerClient";

const SPREAD = 1.03;

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
    let [hasEnteredPrice, setHasEnteredPrice] = useState(false);

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
    const opening = cfds.filter((value) => value.state.getGroup() === StateGroupKey.OPENING);
    const open = cfds.filter((value) => value.state.getGroup() === StateGroupKey.OPEN);
    const closed = cfds.filter((value) => value.state.getGroup() === StateGroupKey.CLOSED);

    return (
        <Container maxWidth="120ch" marginTop="1rem">
            <HStack spacing={5}>
                <VStack>
                    <Wallet walletInfo={walletInfo} />
                    <CurrentPrice priceInfo={priceInfo} />
                    <Grid
                        gridTemplateColumns="max-content auto"
                        padding={5}
                        rowGap={5}
                        columnGap={5}
                        shadow="md"
                        width="100%"
                        alignItems="center"
                    >
                        <Text align={"left"}>Current Price:</Text>
                        <Text>{49000}</Text>

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
                                    setHasEnteredPrice(true);
                                }}
                                value={priceToDisplay(hasEnteredPrice, orderPrice, priceInfo)}
                            />
                            <IconButton
                                aria-label="Reduce"
                                icon={<RepeatIcon />}
                                onClick={() => {
                                    if (priceInfo) {
                                        setOrderPrice((priceInfo.ask * SPREAD).toString());
                                    }
                                }}
                            />
                        </HStack>

                        <Text>Leverage:</Text>
                        <HStack spacing={5}>
                            <Button disabled={true}>x1</Button>
                            <Button disabled={true}>x2</Button>
                            <Button colorScheme="blue" variant="solid">x{5}</Button>
                        </HStack>

                        <GridItem colSpan={2}>
                            <Divider colSpan={2} />
                        </GridItem>

                        <GridItem colSpan={2} textAlign="center">
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

function priceToDisplay(hasEnteredPrice: boolean, orderPrice: string, priceInfo: PriceInfo | null) {
    if (!priceInfo) {
        return format("0");
    }

    if (!hasEnteredPrice) {
        return format((priceInfo.ask * SPREAD).toString());
    }

    return format(orderPrice);
}

function format(val: any) {
    return `$` + val;
}

function parse(val: any) {
    return val.replace(/^\$/, "");
}
