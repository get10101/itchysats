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
    let bid = <Skeleton height="20px" />;
    let ask = <Skeleton height="20px" />;
    let timestamp = <Skeleton height="20px" />;

    if (priceInfo) {
        bid = <Text>{priceInfo.bid} USD</Text>;
        ask = <Text>{priceInfo.ask} USD</Text>;
        timestamp = <Timestamp timestamp={priceInfo.last_updated_at} />;
    }

    return (
        <Box shadow={"md"} marginBottom={5} padding={5}>
            <Center><Text fontWeight={"bold"}>Current Price</Text></Center>
            <HStack>
                <Text align={"left"}>Bid:</Text>
                {bid}
            </HStack>
            <Divider marginTop={2} marginBottom={2} />
            <HStack>
                <Text align={"left"}>Ask:</Text>
                {ask}
            </HStack>
            <Divider marginTop={2} marginBottom={2} />
            <HStack>
                <Text align={"left"}>Updated:</Text>
                {timestamp}
            </HStack>
        </Box>
    );
}
