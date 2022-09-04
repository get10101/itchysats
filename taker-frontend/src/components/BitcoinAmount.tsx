import { forwardRef, Skeleton, Text, TextProps } from "@chakra-ui/react";
import * as React from "react";

interface Props extends TextProps {
    btc: number;
}

/// Displays a BTC value with a fixed precision of 8, also prepending the Bitcoin symbol in front of it.
const BitcoinAmount = forwardRef<Props, "div">((props, ref) => {
    const formatted = props.btc.toFixed(8);
    return (
        <Skeleton isLoaded={Number.isFinite(props.btc)}>
            <Text ref={ref} {...props}>
                ₿{formatted}
            </Text>
        </Skeleton>
    );
});

export default BitcoinAmount;
