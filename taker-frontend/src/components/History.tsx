import { ExternalLinkIcon, Icon } from "@chakra-ui/icons";
import {
    Badge,
    Box,
    Button,
    Center,
    Checkbox,
    Divider,
    GridItem,
    Heading,
    HStack,
    Link,
    Popover,
    PopoverArrow,
    PopoverBody,
    PopoverCloseButton,
    PopoverContent,
    PopoverFooter,
    PopoverHeader,
    PopoverTrigger,
    SimpleGrid,
    Spinner,
    Table,
    Tbody,
    Td,
    Text,
    Tr,
    useColorModeValue,
    useToast,
    VStack,
} from "@chakra-ui/react";
import * as React from "react";
import { useAsync } from "react-async";
import { Cfd, StateGroupKey, StateKey, Tx, TxLabel } from "./Types";

interface HistoryProps {
    cfds: Cfd[];
    title: string;
}

const History = ({ cfds, title }: HistoryProps) => {
    return (
        <VStack spacing={3}>
            <Heading size={"lg"} padding={2}>{title}</Heading>
            <SimpleGrid
                columns={{ sm: 2, md: 4 }}
                gap={4}
            >
                {cfds.map((cfd) => {
                    return (<GridItem rowSpan={1} colSpan={2} key={cfd.order_id}>
                        <CfdDetails cfd={cfd} />
                    </GridItem>);
                })}
            </SimpleGrid>
        </VStack>
    );
};

export default History;

interface CfdDetailsProps {
    cfd: Cfd;
}

async function doPostAction(id: string, action: string) {
    await fetch(
        `/api/cfd/${id}/${action}`,
        { method: "POST", credentials: "include" },
    );
}

const CfdDetails = ({ cfd }: CfdDetailsProps) => {
    const toast = useToast();
    const initialPrice = `$${cfd.initial_price.toLocaleString()}`;
    const quantity = `$${cfd.quantity_usd}`;
    const margin = `₿${Math.round((cfd.margin) * 1_000_000) / 1_000_000}`;
    const liquidationPrice = `$${cfd.liquidation_price}`;
    const pAndL = Math.round((cfd.profit_btc) * 1_000_000) / 1_000_000;
    const expiry = cfd.expiry_timestamp;
    const profit = Math.round((cfd.margin + cfd.profit_btc) * 1_000_000) / 1_000_000;

    const txLock = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Lock);
    const txCommit = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Commit);
    const txRefund = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Refund);
    const txCet = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Cet);
    const txSettled = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Collaborative);

    let { run: postAction, isLoading: isActioning } = useAsync({
        deferFn: async ([orderId, action]: any[]) => {
            try {
                console.log(`Closing: ${orderId} ${action}`);
                await doPostAction(orderId, action);
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

    const disableCloseButton = cfd.state.getGroup() === StateGroupKey.CLOSED
        || [StateKey.OPEN_COMMITTED, StateKey.OUTGOING_SETTLEMENT_PROPOSAL, StateKey.PENDING_CLOSE].includes(
            cfd.state.key,
        );

    return (
        <HStack bg={useColorModeValue("gray.100", "gray.700")} rounded={5}>
            <Center rounded={5} h={"100%"}>
                <Table variant="striped" colorScheme="gray" size="sm">
                    <Tbody>
                        <Tr>
                            <Td><Text as={"b"}>Quantity</Text></Td>
                            <Td>{quantity}</Td>
                        </Tr>
                        <Tr>
                            <Td><Text as={"b"}>Opening price</Text></Td>
                            <Td>{initialPrice}</Td>
                        </Tr>
                        <Tr>
                            <Td><Text as={"b"}>Liquidation</Text></Td>
                            <Td>{liquidationPrice}</Td>
                        </Tr>
                        <Tr>
                            <Td><Text as={"b"}>Margin</Text></Td>
                            <Td>{margin}</Td>
                        </Tr>
                        <Tr>
                            <Td><Text as={"b"}>Unrealized P/L</Text></Td>
                            <Td>{pAndL.toString()}</Td>
                        </Tr>
                    </Tbody>
                </Table>
            </Center>
            <VStack>
                <Badge colorScheme={cfd.state.getColorScheme()}>{cfd.state.getLabel()}</Badge>
                <HStack w={"95%"}>
                    <VStack>
                        <TxIcon tx={txLock} />
                        <Text>Lock</Text>
                    </VStack>
                    {txSettled
                        ? <>
                            <Divider variant={txSettled ? "solid" : "dashed"} />
                            <VStack>
                                <TxIcon tx={txSettled} />
                                <Text>Payout</Text>
                            </VStack>
                        </>
                        : <>
                            <Divider variant={txCommit ? "solid" : "dashed"} />
                            <VStack>
                                <TxIcon tx={txCommit} />
                                <Text>Commit</Text>
                            </VStack>

                            {txRefund
                                ? <>
                                    <Divider variant={txRefund ? "solid" : "dashed"} />
                                    <VStack>
                                        <TxIcon tx={txRefund} />
                                        <Text>Refund</Text>
                                    </VStack>
                                </>
                                : <>
                                    <Divider variant={txCet ? "solid" : "dashed"} />
                                    <VStack>
                                        <TxIcon tx={txCet} />
                                        <Text>Payout</Text>
                                    </VStack>
                                </>}
                        </>}
                </HStack>
                <HStack>
                    <Box w={"45%"}>
                        <Text fontSize={"sm"} align={"left"}>
                            At the current rate you would receive <b>₿ {profit}</b>
                        </Text>
                    </Box>
                    <Box w={"45%"}>
                        <Popover
                            placement="bottom"
                            closeOnBlur={true}
                        >
                            {({ onClose }) => (<>
                                <PopoverTrigger>
                                    <Button colorScheme={"blue"} disabled={disableCloseButton}>Close</Button>
                                </PopoverTrigger>
                                <PopoverContent color="white" bg="blue.800" borderColor="blue.800">
                                    <PopoverHeader pt={4} fontWeight="bold" border="0">
                                        Close your position
                                    </PopoverHeader>
                                    <PopoverArrow />
                                    <PopoverCloseButton />
                                    <PopoverBody>
                                        <Text>
                                            This will force-close your position if your counterparty cannot be reached.
                                            The exchange rate at {expiry}
                                            will determine your profit/losses. It is likely that the rate will change
                                            until then.
                                        </Text>
                                    </PopoverBody>
                                    <PopoverFooter
                                        border="0"
                                        d="flex"
                                        alignItems="center"
                                        justifyContent="space-between"
                                        pb={4}
                                    >
                                        <Button
                                            size="sm"
                                            colorScheme="red"
                                            onClick={async () => {
                                                await postAction(cfd.order_id, "settle");
                                                onClose();
                                            }}
                                            isLoading={isActioning}
                                        >
                                            Close
                                        </Button>
                                        <Checkbox defaultIsChecked>Don't show this again</Checkbox>
                                    </PopoverFooter>
                                </PopoverContent>
                            </>)}
                        </Popover>
                    </Box>
                </HStack>
            </VStack>
        </HStack>
    );
};

const CircleIcon = (props: any) => (
    <Icon viewBox="0 0 200 200" {...props}>
        <path
            stroke="currentColor"
            fill="transparent"
            d="M 100, 100 m -75, 0 a 75,75 0 1,0 150,0 a 75,75 0 1,0 -150,0"
        />
    </Icon>
);

interface TxIconProps {
    tx?: Tx;
}

const TxIcon = ({ tx }: TxIconProps) => {
    const iconColor = useColorModeValue("white.200", "white.600");

    if (!tx) {
        return (<CircleIcon boxSize={5} color={iconColor} />);
    } else if (tx && !tx.url) {
        return (<Spinner mx="2px" color={iconColor} speed="1.65s" />);
    } else {
        return (<Link href={tx.url!} isExternal>
            <ExternalLinkIcon mx="2px" color={iconColor} />
        </Link>);
    }
};
