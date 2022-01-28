import { Text, useColorModeValue } from "@chakra-ui/react";
import * as React from "react";

interface DollarAmountProps {
    amount: number;
    maximumFractionDigits?: number;
    textColor?: {
        light: string;
        dark: string;
    };
}

export default function DollarAmount({ amount, textColor, maximumFractionDigits }: DollarAmountProps) {
    const price = Math.floor(amount * 100.0) / 100.0;

    const colorMode = useColorModeValue(textColor?.light, textColor?.dark);
    // Note : `undefined` will make use of the system local. In our case, the user's browser settings.
    let formatter = Intl.NumberFormat(undefined, {
        maximumFractionDigits: maximumFractionDigits || 0,
    });

    const formattedAmount = formatter.format(price);
    return (
        <Text textColor={colorMode}>
            ${formattedAmount}
        </Text>
    );
}
