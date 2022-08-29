import { ExternalLinkIcon, InfoIcon } from "@chakra-ui/icons";
import {
    Badge,
    GridItem,
    Heading,
    HStack,
    IconButton,
    Link,
    Popover,
    PopoverArrow,
    PopoverBody,
    PopoverContent,
    PopoverTrigger,
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
import BitcoinAmount from "./BitcoinAmount";
import CloseButton from "./CloseButton";
import DollarAmount from "./DollarAmount";

interface HistoryProps {
    cfds: Cfd[];
    title?: string;
    connectedToMaker: ConnectionStatus;
    showExtraInfo: boolean;
}

const History = ({ cfds, title, connectedToMaker, showExtraInfo }: HistoryProps) => {
    return (
        <VStack spacing={3}>
            {title
                && <Heading size={"lg"} padding={2}>{title}</Heading>}
            <SimpleGrid
                columns={{ sm: 2, md: 4 }}
                gap={6}
            >
                {cfds.map((cfd) => {
                    return (
                        <GridItem rowSpan={1} colSpan={2} key={cfd.order_id}>
                            <CfdDetails
                                cfd={cfd}
                                connectedToMaker={connectedToMaker}
                                displayCloseButton={!isClosed(cfd)}
                                showExtraInfo={showExtraInfo}
                            />
                        </GridItem>
                    );
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
    showExtraInfo: boolean;
}

const CfdDetails = ({ cfd, connectedToMaker, displayCloseButton, showExtraInfo }: CfdDetailsProps) => {
    const position = cfd.position;
    const initialPrice = cfd.initial_price;
    const liquidationPrice = cfd.liquidation_price;
    const closing_price = cfd.closing_price || 0;
    const contracts = `${cfd.quantity}`;

    const txLock = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Lock);
    const txCommit = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Commit);
    const txRefund = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Refund);
    const txCet = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Cet);
    const txSettled = cfd.details.tx_url_list.find((tx) => tx.label === TxLabel.Collaborative);

    let [settle, isSettling] = usePostRequest(`/api/cfd/${cfd.order_id}/settle`);
    let [commit, isCommiting] = usePostRequest(`/api/cfd/${cfd.order_id}/commit`);

    const closeButton = connectedToMaker.online
        ? (
            <CloseButton
                request={settle}
                status={isSettling}
                cfd={cfd}
                buttonTitle="Close"
                isForceCloseButton={false}
            />
        )
        : (
            <CloseButton
                request={commit}
                status={isCommiting}
                cfd={cfd}
                buttonTitle="Force-close"
                isForceCloseButton={true}
            />
        );

    let failedCfd = [StateKey.REJECTED, StateKey.SETUP_FAILED].includes(cfd.state.key);

    let profitColors = !cfd || !cfd.profit_btc || cfd.profit_btc === 0
            || failedCfd
        ? { light: "gray.500", dark: "gray.400" }
        : cfd.profit_btc > 0
        ? { light: "green.600", dark: "green.300" }
        : { light: "red.600", dark: "red.300" };

    let profitLabel = isClosed(cfd)
        ? failedCfd ? "Missed P/L 😭" : "Realized P/L"
        : "Unrealized P/L";

    return (
        <HStack
            bg={useColorModeValue("white", "gray.700")}
            boxShadow={"lg"}
            rounded={"md"}
            padding={5}
            alignItems={"stretch"}
        >
            <VStack>
                <Table size="sm" variant={"unstyled"}>
                    <Tbody>
                        <Tr>
                            <Td>
                                <Text as={"b"}>Position</Text>
                            </Td>
                            <Td
                                textAlign="right"
                                textColor={useColorModeValue(position.getColorScheme(), position.getColorScheme())}
                            >
                                {position.key.toString()}
                            </Td>
                        </Tr>
                        <Tr textColor={useColorModeValue(profitColors.light, profitColors.dark)}>
                            <Td>
                                <Text as={"b"}>{profitLabel}</Text>
                            </Td>
                            <Td textAlign="right">
                                <Tooltip label={`${cfd.profit_btc}`} placement={"right"}>
                                    <Skeleton isLoaded={cfd.profit_btc != null}>
                                        {(cfd.profit_percent && cfd.profit_percent > 0) ? "+" : ""}
                                        {cfd.profit_percent}%
                                    </Skeleton>
                                </Tooltip>
                            </Td>
                        </Tr>
                        <Tr textColor={useColorModeValue(profitColors.light, profitColors.dark)}>
                            <Td>
                                <Text as={"b"}>Payout</Text>
                            </Td>
                            <Td textAlign="right">
                                <Skeleton isLoaded={cfd.payout != null}>
                                    <BitcoinAmount btc={cfd.payout ? cfd.payout : 0} />
                                </Skeleton>
                            </Td>
                        </Tr>
                        <Tr>
                            <Td>
                                <Text as={"b"}>Margin</Text>
                            </Td>
                            <Td textAlign="right">
                                <BitcoinAmount btc={cfd.margin} />
                            </Td>
                        </Tr>
                        <Tr>
                            <Td>
                                <Text as={"b"}>Contracts</Text>
                            </Td>
                            <Td textAlign="right">{contracts}</Td>
                        </Tr>
                        <Tr>
                            <Td>
                                <Text as={"b"}>Opening price</Text>
                            </Td>
                            <Td textAlign="right">
                                <DollarAmount amount={initialPrice} />
                            </Td>
                        </Tr>
                        {cfd.closing_price
                            ? (
                                <Tr>
                                    <Td>
                                        <Text as={"b"}>Closing Price</Text>
                                    </Td>
                                    <Td textAlign="right">
                                        <DollarAmount amount={closing_price} />
                                    </Td>
                                </Tr>
                            )
                            : (
                                <Tr>
                                    <Td>
                                        <Text as={"b"}>Liquidation price</Text>
                                    </Td>
                                    <Td textAlign="right">
                                        <DollarAmount amount={liquidationPrice} />
                                    </Td>
                                </Tr>
                            )}
                        <Tr>
                            <Td>
                                <Text as={"b"}>Estimated fees</Text>
                            </Td>
                            <Td textAlign="right">
                                <BitcoinAmount btc={cfd.accumulated_fees} />
                            </Td>
                        </Tr>
                    </Tbody>
                </Table>
            </VStack>
            <VStack justifyContent={"space-between"}>
                <Badge
                    marginTop={5}
                    variant={"outline"}
                    ml={1}
                    fontSize="sm"
                    colorScheme={cfd.state.getColorScheme()}
                >
                    {cfd.state.getLabel()}
                </Badge>
                <Table size="sm" variant={"unstyled"}>
                    <Tbody>
                        <Tr>
                            <Td>
                                <Text>Lock</Text>
                            </Td>
                            <Td>
                                <TxIcon tx={txLock} />
                            </Td>
                        </Tr>
                        {txRefund
                            ? (
                                <Tr>
                                    <Td>
                                        <Text>Refund</Text>
                                    </Td>
                                    <Td>
                                        <TxIcon tx={txRefund} />
                                    </Td>
                                </Tr>
                            )
                            : txCommit || (cfd.state.key === StateKey.OPEN && !connectedToMaker.online)
                            ? (
                                <>
                                    <Tr>
                                        <Td>
                                            <Text>Force</Text>
                                        </Td>
                                        <Td>
                                            <TxIcon tx={txCommit} />
                                        </Td>
                                    </Tr>
                                    <Tr>
                                        <Td>
                                            <Text>Payout</Text>
                                        </Td>
                                        <Td>
                                            <TxIcon tx={txCet} />
                                        </Td>
                                    </Tr>
                                </>
                            )
                            : (
                                <Tr>
                                    <Td>
                                        <Text>Payout</Text>
                                    </Td>
                                    <Td>
                                        <TxIcon tx={txSettled} />
                                    </Td>
                                </Tr>
                            )}
                    </Tbody>
                </Table>
                {displayCloseButton
                    ? (
                        <HStack width={"100%"} paddingBottom={2} justifyContent={"flex-end"}>
                            {closeButton}
                        </HStack>
                    )
                    : <></>}
            </VStack>

            {showExtraInfo
                && (
                    <Popover>
                        <PopoverTrigger>
                            <IconButton
                                bg={"white"}
                                rounded={100}
                                size="xs"
                                aria-label="Info"
                                icon={<InfoIcon color={"black"} />}
                            />
                        </PopoverTrigger>
                        <PopoverContent width={"inherit"}>
                            <PopoverArrow />
                            <PopoverBody>{cfd.order_id}</PopoverBody>
                        </PopoverContent>
                    </Popover>
                )}
        </HStack>
    );
};

interface TxIconProps {
    tx?: Tx;
}

const TxIcon = ({ tx }: TxIconProps) => {
    const colors = !tx
        ? { light: "gray.200", dark: "gray.600" }
        : tx && !tx.url
        ? { light: "gray.200", dark: "gray.600" }
        : { light: "white.200", dark: "white.600" };

    const color = useColorModeValue(colors.light, colors.dark);

    if (!tx) {
        return <ExternalLinkIcon boxSize={5} color={color} />;
    } else if (tx && !tx.url) {
        return <Spinner mx="2px" color={color} speed="1.65s" />;
    } else {
        return (
            <Link href={tx.url!} isExternal>
                <ExternalLinkIcon boxSize={5} color={color} />
            </Link>
        );
    }
};
