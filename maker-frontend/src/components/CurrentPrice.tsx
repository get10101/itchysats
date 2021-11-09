import { HStack, Skeleton, Text, Tooltip } from "@chakra-ui/react";
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
        <Tooltip label={<><Text align={"left"}>Updated:</Text><Timestamp timestamp={last_updated_at!} /></>}>
            <HStack>
                <Text align={"left"} fontWeight={"bold"}>Bid:</Text>
                <Skeleton isLoaded={bid != null}>
                    <Text>{bid} USD</Text>
                </Skeleton>
                <Text align={"left"} fontWeight={"bold"}>Ask:</Text>
                <Skeleton isLoaded={ask != null}>
                    <Text>{ask} USD</Text>
                </Skeleton>
            </HStack>
        </Tooltip>
    );
}
