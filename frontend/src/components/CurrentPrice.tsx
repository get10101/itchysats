import { Box, Center, Divider, HStack, Skeleton, Text } from "@chakra-ui/react";
import React from "react";
import Timestamp from "./Timestamp";
import { PriceInfo } from "./Types";

interface Props {
    priceInfo: PriceInfo | null;
}

export default function CurrentPrice(
    {
        priceInfo,
    }: Props,
) {
    const { ask, bid, last_updated_at } = priceInfo || {};

    return (
        <Box shadow={"md"} marginBottom={5} padding={5}>
            <Center><Text fontWeight={"bold"}>Current Price</Text></Center>
            <HStack>
                <Text align={"left"}>Bid:</Text>
                <Skeleton isLoaded={bid != null}>
                    <Text>{bid} USD</Text>
                </Skeleton>
            </HStack>
            <Divider marginTop={2} marginBottom={2} />
            <HStack>
                <Text align={"left"}>Ask:</Text>
                <Skeleton isLoaded={ask != null}>
                    <Text>{ask} USD</Text>
                </Skeleton>
            </HStack>
            <Divider marginTop={2} marginBottom={2} />
            <HStack>
                <Text align={"left"}>Updated:</Text>
                <Skeleton isLoaded={last_updated_at != null}>
                    <Timestamp timestamp={last_updated_at!} />
                </Skeleton>
            </HStack>
        </Box>
    );
}
