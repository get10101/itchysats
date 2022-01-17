import { Skeleton, Text } from "@chakra-ui/react";
import * as React from "react";

interface Props {
    btc: number;
}

/// Displays a BTC value with a fixed precision of 8, also prepending the Bitcoin symbol in front of it.
export default function BitcoinAmount({ btc }: Props) {
    const formatted = btc.toFixed(8);

    return <Skeleton isLoaded={Number.isFinite(btc)}>
        <Text>₿{formatted}</Text>
    </Skeleton>;
}
