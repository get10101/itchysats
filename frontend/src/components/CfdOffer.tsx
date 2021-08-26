import { Box, SimpleGrid, Text, VStack } from "@chakra-ui/react";
import React from "react";
import { Offer } from "./Types";

interface CfdOfferProps {
    offer: Offer;
}

function CfdOffer(
    {
        offer,
    }: CfdOfferProps,
) {
    return (
        <Box borderRadius={"md"} borderColor={"blue.800"} borderWidth={2} bg={"gray.50"}>
            <VStack>
                <Box bg="blue.800" w="100%">
                    <Text padding={2} color={"white"} fontWeight={"bold"}>Current CFD Sell Offer</Text>
                </Box>
                <SimpleGrid padding={5} columns={2} spacing={5}>
                    <Text>ID</Text>
                    <Text
                        overflow="hidden"
                        textOverflow="ellipsis"
                        whiteSpace="nowrap"
                        _hover={{ overflow: "visible" }}
                    >
                        {offer.id}
                    </Text>
                    <Text>Trading Pair</Text>
                    <Text>{offer.trading_pair}</Text>
                    <Text>Price</Text>
                    <Text>{offer.price}</Text>
                    <Text>Min Quantity</Text>
                    <Text>{offer.min_quantity}</Text>
                    <Text>Max Quantity</Text>
                    <Text>{offer.max_quantity}</Text>
                    <Text>Leverage</Text>
                    <Text>{offer.leverage}</Text>
                    <Text>Liquidation Price</Text>
                    <Text
                        overflow="hidden"
                        textOverflow="ellipsis"
                        whiteSpace="nowrap"
                        _hover={{ overflow: "visible" }}
                    >
                        {offer.liquidation_price}
                    </Text>
                </SimpleGrid>
            </VStack>
        </Box>
    );
}

export default CfdOffer;
