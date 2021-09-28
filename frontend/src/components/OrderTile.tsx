import { Box, HStack, Text, VStack } from "@chakra-ui/react";
import React from "react";
import { Order } from "./Types";

interface OrderProps {
    order: Order;
}

function OrderTile(
    {
        order,
    }: OrderProps,
) {
    const labelWidth = 140;

    return (
        <Box borderRadius={"md"} borderColor={"blue.800"} borderWidth={2} bg={"gray.50"}>
            <VStack>
                <Box bg="blue.800" w="100%">
                    <Text padding={2} color={"white"} fontWeight={"bold"}>Current CFD Sell Order</Text>
                </Box>
                <VStack padding={5} spacing={5} align={"stretch"}>
                    <HStack>
                        <Text width={labelWidth}>ID</Text>
                        <Text
                            overflow="hidden"
                            textOverflow="ellipsis"
                            whiteSpace="nowrap"
                            _hover={{ overflow: "visible" }}
                        >
                            {order.id}
                        </Text>
                    </HStack>
                    <HStack>
                        <Text width={labelWidth}>Trading Pair</Text>
                        <Text>{order.trading_pair}</Text>
                    </HStack>
                    <HStack>
                        <Text width={labelWidth}>Price</Text>
                        <Text>{order.price}</Text>
                    </HStack>
                    <HStack>
                        <Text width={labelWidth}>Min Quantity</Text>
                        <Text>{order.min_quantity}</Text>
                    </HStack>
                    <HStack>
                        <Text width={labelWidth}>Max Quantity</Text>
                        <Text>{order.max_quantity}</Text>
                    </HStack>
                    <HStack>
                        <Text width={labelWidth}>Leverage</Text>
                        <Text>{order.leverage}</Text>
                    </HStack>
                    <HStack>
                        <Text width={labelWidth}>Liquidation Price</Text>
                        <Text
                            overflow="hidden"
                            textOverflow="ellipsis"
                            whiteSpace="nowrap"
                            _hover={{ overflow: "visible" }}
                        >
                            {order.liquidation_price}
                        </Text>
                    </HStack>
                </VStack>
            </VStack>
        </Box>
    );
}

export default OrderTile;
