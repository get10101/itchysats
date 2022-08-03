import {
    Box,
    Button,
    Checkbox,
    CheckboxGroup,
    Container,
    Divider,
    Grid,
    GridItem,
    HStack,
    Stack,
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
import { useAsync } from "@react-hookz/web";
import React, { useEffect, useState } from "react";
import { useEventSource } from "react-sse-hooks";
import { CfdTable } from "./components/cfdtables/CfdTable";
import CurrencyInputField from "./components/CurrencyInputField";
import CurrentPrice from "./components/CurrentPrice";
import createErrorToast from "./components/ErrorToast";
import useLatestEvent from "./components/Hooks";
import OrderTile from "./components/OrderTile";
import { Cfd, intoCfd, MakerOffer, PriceInfo, StateGroupKey, WalletInfo } from "./components/Types";
import Wallet from "./components/Wallet";
import { CfdNewOfferParamsPayload, putCfdNewOfferParamsRequest, triggerWalletSync } from "./MakerClient";

const SPREAD_ASK = 1.01;
const SPREAD_BID = 0.99;

export default function App() {
    document.title = "Hermes Maker";

    // TODO: make this dynamic or subscribe to both
    let source = useEventSource({ source: "/api/btcusd/feed", options: { withCredentials: true } });

    let [leverages, setLeverages] = useState(["1", "2", "3"]);

    const cfdsOrUndefined = useLatestEvent<Cfd[]>(source, "cfds", intoCfd);
    let cfds = cfdsOrUndefined ? cfdsOrUndefined! : [];
    const makerLongOrder = useLatestEvent<MakerOffer>(source, "long_offer");
    const makerShortOrder = useLatestEvent<MakerOffer>(source, "short_offer");
    const walletInfo = useLatestEvent<WalletInfo>(source, "wallet");
    const priceInfo = useLatestEvent<PriceInfo>(source, "quote");

    const toast = useToast();

    let [minQuantity, setMinQuantity] = useState<string>("100");
    let [maxQuantity, setMaxQuantity] = useState<string>("1000");
    let [shortPrice, setShortPrice] = useState<string>("0");
    let [longPrice, setLongPrice] = useState<string>("0");
    let [autoRefreshShort, setAutoRefreshShort] = useState(true);
    let [autoRefreshLong, setAutoRefreshLong] = useState(true);

    useEffect(() => {
        if (autoRefreshShort && priceInfo) {
            setShortPrice((priceInfo.ask * SPREAD_ASK).toFixed(2).toString());
        }
    }, [priceInfo, autoRefreshShort]);

    useEffect(() => {
        if (autoRefreshLong && priceInfo) {
            setLongPrice((priceInfo.bid * SPREAD_BID).toFixed(2).toString());
        }
    }, [priceInfo, autoRefreshLong]);

    const [{ status: newCfdStatus }, { execute: makeNewCfdSellOrder }] = useAsync(
        async (payload: CfdNewOfferParamsPayload) => {
            try {
                await putCfdNewOfferParamsRequest(payload);
            } catch (e) {
                createErrorToast(toast, e);
            }
        },
    );
    const isCreatingNewCfdOrder = newCfdStatus === "loading";

    const [{ status: walletStatus }, { execute: syncWallet }] = useAsync(
        async () => {
            try {
                await triggerWalletSync();
            } catch (e) {
                createErrorToast(toast, e);
            }
        },
    );

    const isSyncingWallet = walletStatus === "loading";

    const pendingOrders = cfds.filter((value) => value.state.getGroup() === StateGroupKey.PENDING_ORDER);
    const pendingSettlements = cfds.filter((value) => value.state.getGroup() === StateGroupKey.PENDING_SETTLEMENT);
    const pendingRollovers = cfds.filter((value) => value.state.getGroup() === StateGroupKey.PENDING_ROLLOVER);
    const opening = cfds.filter((value) => value.state.getGroup() === StateGroupKey.OPENING);
    const open = cfds.filter((value) => value.state.getGroup() === StateGroupKey.OPEN);
    const closed = cfds.filter((value) => value.state.getGroup() === StateGroupKey.CLOSED);

    return (
        <Container maxWidth="120ch" marginTop="1rem">
            <HStack spacing={5}>
                <VStack>
                    <Wallet
                        walletInfo={walletInfo}
                        syncWallet={async () => {
                            await syncWallet();
                        }}
                        isSyncingWallet={isSyncingWallet}
                    />

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

                        <Text>Max Quantity:</Text>
                        <CurrencyInputField
                            onChange={(valueString: string) => setMaxQuantity(parse(valueString))}
                            value={format(maxQuantity)}
                        />

                        <Text>Maker Short Price:</Text>
                        <HStack>
                            <CurrencyInputField
                                onChange={(valueString: string) => {
                                    setShortPrice(parse(valueString));
                                    setAutoRefreshShort(false);
                                }}
                                value={format(shortPrice)}
                            />
                            <HStack>
                                <Switch
                                    id="auto-refresh-short"
                                    isChecked={autoRefreshShort}
                                    onChange={() => setAutoRefreshShort(!autoRefreshShort)}
                                />
                                <Text>Auto-refresh</Text>
                            </HStack>
                        </HStack>

                        <Text>Maker Long Price:</Text>
                        <HStack>
                            <CurrencyInputField
                                onChange={(valueString: string) => {
                                    setLongPrice(parse(valueString));
                                    setAutoRefreshLong(false);
                                }}
                                value={format(longPrice)}
                            />
                            <HStack>
                                <Switch
                                    id="auto-refresh-long"
                                    isChecked={autoRefreshLong}
                                    onChange={() => setAutoRefreshLong(!autoRefreshLong)}
                                />
                                <Text>Auto-refresh</Text>
                            </HStack>
                        </HStack>

                        <Text>Leverage:</Text>
                        <CheckboxGroup
                            defaultValue={leverages}
                            onChange={(value) => setLeverages(value.map((v) => v.toString()))}
                        >
                            <Stack spacing={[1, 5]} direction={["column", "row"]}>
                                <Checkbox value="1">1x</Checkbox>
                                <Checkbox value="2">2x</Checkbox>
                                <Checkbox value="3">3x</Checkbox>
                            </Stack>
                        </CheckboxGroup>

                        <GridItem colSpan={2}>
                            <Divider colSpan={2} />
                        </GridItem>

                        <GridItem colSpan={2} textAlign="center">
                            <Button
                                disabled={isCreatingNewCfdOrder || shortPrice === "0"}
                                variant={"solid"}
                                colorScheme={"blue"}
                                onClick={async () => {
                                    let payload: CfdNewOfferParamsPayload = {
                                        price_short: Number.parseFloat(shortPrice),
                                        price_long: Number.parseFloat(longPrice),
                                        min_quantity: Number.parseFloat(minQuantity),
                                        max_quantity: Number.parseFloat(maxQuantity),
                                        // TODO: Populate funding rate from the UI
                                        daily_funding_rate_short: (0.00002283 * 24), // annualized 20% by default to have some values
                                        // TODO: Populate funding rate from the UI
                                        daily_funding_rate_long: (-0.00002283 * 24), // annualized 20% by default to have some values
                                        tx_fee_rate: Number.parseFloat("1"),
                                        // TODO: This is is in sats which is not really in line with other APIs for the maker
                                        opening_fee: Number.parseFloat("100"),
                                        leverage_choices: leverages.map((val) => Number.parseInt(val)),
                                    };
                                    await makeNewCfdSellOrder(payload);
                                }}
                            >
                                {makerLongOrder ? "Update Offers" : "Create Offers"}
                            </Button>
                        </GridItem>
                    </Grid>
                </VStack>
                <HStack>
                    {makerShortOrder && <OrderTile maker_offer={makerShortOrder} />}
                    {makerLongOrder && <OrderTile maker_offer={makerLongOrder} />}
                </HStack>
                <Box width="40%" />
            </HStack>

            <Tabs marginTop={5}>
                <TabList>
                    <Tab>Open [{open.length}]</Tab>
                    <Tab>Pending Orders [{pendingOrders.length}]</Tab>
                    <Tab>Pending Settlements [{pendingSettlements.length}]</Tab>
                    <Tab>Pending Roll Overs [{pendingRollovers.length}]</Tab>
                    <Tab>Opening [{opening.length}]</Tab>
                    <Tab>Closed [{closed.length}]</Tab>
                </TabList>

                <TabPanels>
                    <TabPanel>
                        <CfdTable data={open} />
                    </TabPanel>
                    <TabPanel>
                        <CfdTable data={pendingOrders} />
                    </TabPanel>
                    <TabPanel>
                        <CfdTable data={pendingSettlements} />
                    </TabPanel>
                    <TabPanel>
                        <CfdTable data={pendingRollovers} />
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
