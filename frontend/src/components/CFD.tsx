import { Box, Button, SimpleGrid, Text, VStack } from "@chakra-ui/react";
import React from "react";

interface CFDProps {
    trading_pair?: "BTC/USD";
    position?: "buy" | "sell";
    number: number;
    amount: number;
    liquidation_price: number;
    profit: number;
    creation_date: Date;
    status: "requested" | "ongoing" | "pending" | "liquidated";
}

function CFD(
    {
        trading_pair = "BTC/USD",
        position = "buy",
        number,
        amount,
        liquidation_price,
        profit,
        creation_date,
        status,
    }: CFDProps,
) {
    return (
        <Box shadow={"md"} bg="gray.100" borderRadius="lg">
            <VStack>
                <Box bg="blue.100" w="100%">
                    <Text>CFD #{number}</Text>
                </Box>
                <SimpleGrid padding={5} columns={2} spacing={5}>
                    <Text>Trading Pair</Text>
                    <Text>{trading_pair}</Text>
                    <Text>Position</Text>
                    <Text>{position}</Text>
                    <Text>Amount</Text>
                    <Text>{amount}</Text>
                    <Text>Liquidation Price</Text>
                    <Text>{liquidation_price}</Text>
                    <Text>Profit</Text>
                    <Text>{profit}</Text>
                    <Text>Open since</Text>
                    {/* TODO: Format date in a more compact way */}
                    <Text>{creation_date.toString()}</Text>
                    <Text>Status</Text>
                    <Text>{status}</Text>
                </SimpleGrid>
                if (status === "ongoing") {<Button colorScheme="blue" variant="solid">Close</Button>}
            </VStack>
        </Box>
    );
}

export default CFD;
