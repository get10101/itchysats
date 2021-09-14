import { Box, Button, SimpleGrid, Text, VStack } from "@chakra-ui/react";
import React from "react";
import { Cfd } from "./Types";

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
                        {(new Date(cfd.state_transition_unix_timestamp * 1000).toString())}
                    </Text>
                    <Text>Status</Text>
                    <Text>{cfd.state}</Text>
                </SimpleGrid>
                {cfd.state === "Open"
                    && <Box paddingBottom={5}><Button colorScheme="blue" variant="solid">Close</Button></Box>}
            </VStack>
        </Box>
    );
}
