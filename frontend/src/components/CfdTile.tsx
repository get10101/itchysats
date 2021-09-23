import { Box, Button, HStack, SimpleGrid, Text, useToast, VStack } from "@chakra-ui/react";
import React from "react";
import { useAsync } from "react-async";
import { postAcceptOrder, postRejectOrder } from "../MakerClient";
import { Cfd, unixTimestampToDate } from "./Types";

interface CfdTileProps {
    index: number;
    cfd: Cfd;
}

export default function CfdTile(
    {
        index,
        cfd,
    }: CfdTileProps,
) {
    const toast = useToast();

    let { run: acceptOrder, isLoading: isAccepting } = useAsync({
        deferFn: async ([args]: any[]) => {
            try {
                let payload = {
                    order_id: args.order_id,
                };
                await postAcceptOrder(payload);
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

    let { run: rejectOrder, isLoading: isRejecting } = useAsync({
        deferFn: async ([args]: any[]) => {
            try {
                let payload = {
                    order_id: args.order_id,
                };
                await postRejectOrder(payload);
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

    let actionButtons;
    if (cfd.state === "Open") {
        actionButtons = <Box paddingBottom={5}>
            <Button colorScheme="blue" variant="solid">
                Close
            </Button>
        </Box>;
    } else if (cfd.state == "Requested") {
        actionButtons = (
            <HStack>
                <Box paddingBottom={5}>
                    <Button
                        colorScheme="blue"
                        variant="solid"
                        onClick={async () => acceptOrder(cfd)}
                        isLoading={isAccepting}
                    >
                        Accept
                    </Button>
                </Box>
                <Box paddingBottom={5}>
                    <Button
                        colorScheme="blue"
                        variant="solid"
                        onClick={async () => rejectOrder(cfd)}
                        isLoading={isRejecting}
                    >
                        Reject
                    </Button>
                </Box>
            </HStack>
        );
    }

    return (
        <Box borderRadius={"md"} borderColor={"blue.800"} borderWidth={2} bg={"gray.50"}>
            <VStack>
                <Box bg="blue.800" w="100%">
                    <Text padding={2} color={"white"} fontWeight={"bold"}>CFD #{index}</Text>
                </Box>
                <SimpleGrid padding={5} columns={2} spacing={5}>
                    <Text>Trading Pair</Text>
                    <Text>{cfd.trading_pair}</Text>
                    <Text>Position</Text>
                    <Text>{cfd.position}</Text>
                    <Text>CFD Price</Text>
                    <Text>{cfd.initial_price}</Text>
                    <Text>Leverage</Text>
                    <Text>{cfd.leverage}</Text>
                    <Text>Quantity</Text>
                    <Text>{cfd.quantity_usd}</Text>
                    <Text>Margin</Text>
                    <Text>{cfd.margin}</Text>
                    <Text>Liquidation Price</Text>
                    <Text
                        overflow="hidden"
                        textOverflow="ellipsis"
                        whiteSpace="nowrap"
                        _hover={{ overflow: "visible" }}
                    >
                        {cfd.liquidation_price}
                    </Text>
                    <Text>Profit</Text>
                    <Text>{cfd.profit_usd}</Text>
                    <Text>Open since</Text>
                    {/* TODO: Format date in a more compact way */}
                    <Text>
                        {unixTimestampToDate(cfd.state_transition_timestamp).toString()}
                    </Text>
                    <Text>Status</Text>
                    <Text>{cfd.state}</Text>
                </SimpleGrid>
                {actionButtons}
            </VStack>
        </Box>
    );
}
