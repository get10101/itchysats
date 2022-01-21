import { ExternalLinkIcon } from "@chakra-ui/icons";
import {
    Badge,
    GridItem,
    Heading,
    HStack,
    Link,
    SimpleGrid,
    Skeleton,
    Spinner,
    Table,
    Tbody,
    Td,
    Text,
    Tooltip,
    Tr,
    useColorModeValue,
    VStack,
} from "@chakra-ui/react";
import * as React from "react";
import { Cfd, ConnectionStatus, isClosed, StateKey, Tx, TxLabel } from "../types";
import usePostRequest from "../usePostRequest";
import CloseButton from "./CloseButton";

interface HistoryProps {
    cfds: Cfd[];
    title?: string;
    connectedToMaker: ConnectionStatus;
}

const History = ({ cfds, title, connectedToMaker }: HistoryProps) => {
    return (
        <VStack spacing={3}>
            {title
                && <Heading size={"lg"} padding={2}>{title}</Heading>}
            <SimpleGrid
                columns={{ sm: 2, md: 4 }}
                gap={6}
            >
                {cfds.map((cfd) => {
                    return (<GridItem rowSpan={1} colSpan={2} key={cfd.order_id}>
                        <CfdDetails
                            cfd={cfd}
                            connectedToMaker={connectedToMaker}
                            displayCloseButton={!isClosed(cfd)}
                        />
                    </GridItem>);
                })}
            </SimpleGrid>
        </VStack>
    );
};

export default History;

interface CfdDetailsProps {
    cfd: Cfd;
    connectedToMaker: ConnectionStatus;
    displayCloseButton: boolean;
}

const CfdDetails = ({ cfd, connectedToMaker, displayCloseButton }: CfdDetailsProps) => {
    const initialPrice = `$${cfd.initial_price.toLocaleString()}`;
    const margin = `₿${Math.round((cfd.margin) * 1_000_000) / 1_000_000}`;
    const payout = `₿${Math.round((cfd.payout ? cfd.payout : 0) * 1_000_000) / 1_000_000}`;
    const liquidationPrice = `$${cfd.liquidation_price}`;
    const contracts = `${cfd.quantity_usd}`;

    const txLock = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Lock);
    const txCommit = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Commit);
    const txRefund = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Refund);
    const txCet = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Cet);
    const txSettled = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Collaborative);

    const accumulatedCosts = `₿${cfd.accumulated_fees}`;

    let [settle, isSettling] = usePostRequest(`/api/cfd/${cfd.order_id}/settle`);
    let [commit, isCommiting] = usePostRequest(`/api/cfd/${cfd.order_id}/commit`);

    const closeButton = connectedToMaker.online
        ? <CloseButton
            request={settle}
            status={isSettling}
            cfd={cfd}
            buttonTitle="Close"
            isForceCloseButton={false}
        />
        : <CloseButton
            request={commit}
            status={isCommiting}
            cfd={cfd}
            buttonTitle="Force-close"
            isForceCloseButton={true}
        />;

    let profitColors = !cfd || !cfd.profit_btc || cfd.profit_btc === 0
        ? ["gray.500", "gray.400"]
        : cfd.profit_btc > 0
        ? ["green.600", "green.300"]
        : ["red.600", "red.300"];

    return (
        <HStack
            bg={useColorModeValue("white", "gray.700")}
            rounded={"md"}
            padding={5}
            alignItems={"stretch"}
            boxShadow={"lg"}
        >
            <VStack>
                <Table size="sm" variant={"unstyled"}>
                    <Tbody>
                        <Tr textColor={useColorModeValue(profitColors[0], profitColors[1])}>
                            <Td><Text as={"b"}>Unrealized P/L</Text></Td>
                            <Td textAlign="right">
                                <Tooltip label={`${cfd.profit_btc}`} placement={"right"}>
                                    <Skeleton isLoaded={cfd.profit_btc != null}>
                                        {(cfd.profit_percent && cfd.profit_percent > 0) ? "+" : ""}
                                        {cfd.profit_percent}%
                                    </Skeleton>
                                </Tooltip>
                            </Td>
                        </Tr>
                        <Tr textColor={useColorModeValue(profitColors[0], profitColors[1])}>
                            <Td><Text as={"b"}>Payout</Text></Td>
                            <Td textAlign="right">
                                <Skeleton isLoaded={cfd.payout != null}>
                                    {payout}
                                </Skeleton>
                            </Td>
                        </Tr>
                        <Tr>
                            <Td><Text as={"b"}>Margin</Text></Td>
                            <Td textAlign="right">{margin}</Td>
                        </Tr>
                        <Tr>
                            <Td><Text as={"b"}>Contracts</Text></Td>
                            <Td textAlign="right">{contracts}</Td>
                        </Tr>
                        <Tr>
                            <Td><Text as={"b"}>Opening price</Text></Td>
                            <Td textAlign="right">{initialPrice}</Td>
                        </Tr>
                        <Tr>
                            <Td><Text as={"b"}>Liquidation</Text></Td>
                            <Td textAlign="right">{liquidationPrice}</Td>
                        </Tr>
                    </Tbody>
                </Table>
                <Table size="sm" variant={"unstyled"}>
                    <Tbody>
                        <Tr>
                            <Td><Text as={"b"}>Total fees</Text></Td>
                            <Td textAlign="right">{accumulatedCosts}</Td>
                        </Tr>
                    </Tbody>
                </Table>
            </VStack>
            <VStack justifyContent={"space-between"}>
                <Badge marginTop={5} variant={"outline"} ml={1} fontSize="sm" colorScheme={cfd.state.getColorScheme()}>
                    {cfd.state.getLabel()}
                </Badge>
                <Table size="sm" variant={"unstyled"}>
                    <Tbody>
                        <Tr>
                            <Td><Text>Lock</Text></Td>
                            <Td><TxIcon tx={txLock} /></Td>
                        </Tr>
                        {txRefund
                            ? <Tr>
                                <Td><Text>Refund</Text></Td>
                                <Td><TxIcon tx={txRefund} /></Td>
                            </Tr>
                            : txCommit || (cfd.state.key === StateKey.OPEN && !connectedToMaker.online)
                            ? <>
                                <Tr>
                                    <Td><Text>Force</Text></Td>
                                    <Td><TxIcon tx={txCommit} /></Td>
                                </Tr>
                                <Tr>
                                    <Td><Text>Payout</Text></Td>
                                    <Td><TxIcon tx={txCet} /></Td>
                                </Tr>
                            </>
                            : <Tr>
                                <Td><Text>Payout</Text></Td>
                                <Td><TxIcon tx={txSettled} /></Td>
                            </Tr>}
                    </Tbody>
                </Table>
                {displayCloseButton
                    ? <HStack width={"100%"} paddingBottom={2} justifyContent={"flex-end"}>
                        {closeButton}
                    </HStack>
                    : <></>}
            </VStack>
        </HStack>
    );
};

interface TxIconProps {
    tx?: Tx;
}

const TxIcon = ({ tx }: TxIconProps) => {
    const colors = !tx
        ? ["gray.200", "gray.600"]
        : tx && !tx.url
        ? ["gray.200", "gray.600"]
        : ["white.200", "white.600"];

    const color = useColorModeValue(colors[0], colors[1]);

    if (!tx) {
        return (<ExternalLinkIcon boxSize={5} color={color} />);
    } else if (tx && !tx.url) {
        return (<Spinner mx="2px" color={color} speed="1.65s" />);
    } else {
        return (<Link href={tx.url!} isExternal>
            <ExternalLinkIcon boxSize={5} color={color} />
        </Link>);
    }
};
