import { ExternalLinkIcon, Icon } from "@chakra-ui/icons";
import {
    Badge,
    Center,
    Divider,
    GridItem,
    Heading,
    HStack,
    Link,
    SimpleGrid,
    Spinner,
    Table,
    Tbody,
    Td,
    Text,
    Tr,
    useColorModeValue,
    VStack,
} from "@chakra-ui/react";
import * as React from "react";
import { Cfd, ConnectionStatus, Tx, TxLabel } from "../types";
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
                gap={4}
            >
                {cfds.map((cfd) => {
                    return (<GridItem rowSpan={1} colSpan={2} key={cfd.order_id}>
                        <CfdDetails cfd={cfd} connectedToMaker={connectedToMaker} />
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
}

const CfdDetails = ({ cfd, connectedToMaker }: CfdDetailsProps) => {
    const initialPrice = `$${cfd.initial_price.toLocaleString()}`;
    const quantity = `$${cfd.quantity_usd}`;
    const margin = `₿${Math.round((cfd.margin) * 1_000_000) / 1_000_000}`;
    const liquidationPrice = `$${cfd.liquidation_price}`;

    const pAndLNumber = Math.round((cfd.profit_btc) * 1_000_000) / 1_000_000;
    const pAndL = pAndLNumber < 0 ? `-₿${Math.abs(pAndLNumber)}` : `₿${Math.abs(pAndLNumber)}`;

    const payout = `₿${Math.round((cfd.margin + cfd.profit_btc) * 1_000_000) / 1_000_000}`;

    const txLock = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Lock);
    const txCommit = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Commit);
    const txRefund = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Refund);
    const txCet = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Cet);
    const txSettled = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Collaborative);

    let [settle, isSettling] = usePostRequest(`/api/cfd/${cfd.order_id}/settle`);
    let [commit, isCommiting] = usePostRequest(`/api/cfd/${cfd.order_id}/commit`);

    const closeButton = connectedToMaker.online
        ? <CloseButton request={settle} status={isSettling} cfd={cfd} buttonTitle="Close" isForceCloseButton={false} />
        : <CloseButton
            request={commit}
            status={isCommiting}
            cfd={cfd}
            buttonTitle="Force Close"
            isForceCloseButton={true}
        />;

    return (
        <HStack bg={useColorModeValue("gray.100", "gray.700")} rounded={5} padding={2}>
            <Center>
                <Table variant="striped" colorScheme="gray" size="sm">
                    <Tbody>
                        <Tr>
                            <Td><Text as={"b"}>Quantity</Text></Td>
                            {/*TODO: Fix textAlign right hacks by using a grid instead ... */}
                            <Td textAlign="right">{quantity}</Td>
                        </Tr>
                        <Tr>
                            <Td><Text as={"b"}>Opening price</Text></Td>
                            <Td textAlign="right">{initialPrice}</Td>
                        </Tr>
                        <Tr>
                            <Td><Text as={"b"}>Liquidation</Text></Td>
                            <Td textAlign="right">{liquidationPrice}</Td>
                        </Tr>
                        <Tr>
                            <Td><Text as={"b"}>Margin</Text></Td>
                            <Td textAlign="right">{margin}</Td>
                        </Tr>
                        <Tr>
                            <Td><Text as={"b"}>Unrealized P/L</Text></Td>
                            <Td textAlign="right">{pAndL}</Td>
                        </Tr>
                        <Tr>
                            <Td><Text as={"b"}>Payout</Text></Td>
                            <Td textAlign="right">{payout}</Td>
                        </Tr>
                    </Tbody>
                </Table>
            </Center>
            <VStack>
                <Badge colorScheme={cfd.state.getColorScheme()}>{cfd.state.getLabel()}</Badge>
                <HStack>
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
                    {closeButton}
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
