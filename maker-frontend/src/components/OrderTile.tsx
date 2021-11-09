import { Box, Grid, Text, VStack } from "@chakra-ui/react";
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
                <Grid gridTemplateColumns="max-content auto" padding={5} rowGap={2}>
                    <Text width={labelWidth}>ID</Text>
                    <Text whiteSpace="nowrap">{order.id}</Text>

                    <Text width={labelWidth}>Trading Pair</Text>
                    <Text>{order.trading_pair}</Text>

                    <Text width={labelWidth}>Price</Text>
                    <Text>{order.price}</Text>

                    <Text width={labelWidth}>Min Quantity</Text>
                    <Text>{order.min_quantity}</Text>

                    <Text width={labelWidth}>Max Quantity</Text>
                    <Text>{order.max_quantity}</Text>

                    <Text width={labelWidth}>Leverage</Text>
                    <Text>{order.leverage}</Text>

                    <Text width={labelWidth}>Liquidation Price</Text>
                    <Text whiteSpace="nowrap">{order.liquidation_price}</Text>
                </Grid>
            </VStack>
        </Box>
    );
}

export default OrderTile;
