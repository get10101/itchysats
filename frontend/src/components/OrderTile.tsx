import { Box, SimpleGrid, Text, VStack } from "@chakra-ui/react";
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
    return (
        <Box borderRadius={"md"} borderColor={"blue.800"} borderWidth={2} bg={"gray.50"}>
            <VStack>
                <Box bg="blue.800" w="100%">
                    <Text padding={2} color={"white"} fontWeight={"bold"}>Current CFD Sell Order</Text>
                </Box>
                <SimpleGrid padding={5} columns={2} spacing={5}>
                    <Text>ID</Text>
                    <Text
                        overflow="hidden"
                        textOverflow="ellipsis"
                        whiteSpace="nowrap"
                        _hover={{ overflow: "visible" }}
                    >
                        {order.id}
                    </Text>
                    <Text>Trading Pair</Text>
                    <Text>{order.trading_pair}</Text>
                    <Text>Price</Text>
                    <Text>{order.price}</Text>
                    <Text>Min Quantity</Text>
                    <Text>{order.min_quantity}</Text>
                    <Text>Max Quantity</Text>
                    <Text>{order.max_quantity}</Text>
                    <Text>Leverage</Text>
                    <Text>{order.leverage}</Text>
                    <Text>Liquidation Price</Text>
                    <Text
                        overflow="hidden"
                        textOverflow="ellipsis"
                        whiteSpace="nowrap"
                        _hover={{ overflow: "visible" }}
                    >
                        {order.liquidation_price}
                    </Text>
                </SimpleGrid>
            </VStack>
        </Box>
    );
}

export default OrderTile;
