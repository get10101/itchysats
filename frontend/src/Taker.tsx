import {
    Button,
    Container,
    Flex, Grid, GridItem,
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
import React, {useState} from "react";
import {useAsync} from "react-async";
import {useEventSource} from "react-sse-hooks";
import {CfdTable} from "./components/cfdtables/CfdTable";
import CurrencyInputField from "./components/CurrencyInputField";
import useLatestEvent from "./components/Hooks";
import {Cfd, Order, WalletInfo} from "./components/Types";
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
    let res = await fetch(`/api/cfd`, {method: "POST", body: JSON.stringify(payload)});

    if (!res.status.toString().startsWith("2")) {
        throw new Error("failed to create new CFD order request: " + res.status + ", " + res.statusText);
    }
}

async function getMargin(payload: MarginRequestPayload): Promise<MarginResponse> {
    let res = await fetch(`/api/calculate/margin`, {method: "POST", body: JSON.stringify(payload)});

    if (!res.status.toString().startsWith("2")) {
        throw new Error("failed to create new CFD order request: " + res.status + ", " + res.statusText);
    }

    return res.json();
}

export default function App() {
    let source = useEventSource({source: "/api/feed"});

    const cfdsOrUndefined = useLatestEvent<Cfd[]>(source, "cfds");
    let cfds = cfdsOrUndefined ? cfdsOrUndefined! : [];
    const order = useLatestEvent<Order>(source, "order");
    const walletInfo = useLatestEvent<WalletInfo>(source, "wallet");

    const toast = useToast();
    let [quantity, setQuantity] = useState("0");
    let [margin, setMargin] = useState("0");

    let {run: calculateMargin} = useAsync({
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

    let {run: makeNewOrderRequest, isLoading: isCreatingNewOrderRequest} = useAsync({
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

    const runningStates = ["Request sent", "Requested", "Contract Setup", "Pending Open"];
    const running = cfds.filter((value) => runningStates.includes(value.state));
    const closedStates = ["Rejected", "Closed"];
    const closed = cfds.filter((value) => closedStates.includes(value.state));
    // TODO: remove this. It just helps to detect immediately if we missed a state.
    const unsorted = cfds.filter((value) =>
        !runningStates.includes(value.state) && !closedStates.includes(value.state)
    );

    return (
        <Container maxWidth="120ch" marginTop="1rem">
            <VStack>
                <Grid templateColumns="repeat(6, 1fr)" gap={4}>
                    <GridItem colStart={1} colSpan={2}>
                        <Wallet walletInfo={walletInfo}/>
                        <VStack shadow={"md"} padding={5} align="stretch" spacing={4}>
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
                <Tabs>
                    <TabList>
                        <Tab>Running [{running.length}]</Tab>
                        <Tab>Closed [{closed.length}]</Tab>
                        <Tab>Unsorted [{unsorted.length}] (should be empty)</Tab>
                    </TabList>

                    <TabPanels>
                        <TabPanel>
                            <CfdTable data={running}/>
                        </TabPanel>
                        <TabPanel>
                            <CfdTable data={closed}/>
                        </TabPanel>
                        <TabPanel>
                            <CfdTable data={unsorted}/>
                        </TabPanel>
                    </TabPanels>
                </Tabs>
            </VStack>
        </Container>
    );
}
