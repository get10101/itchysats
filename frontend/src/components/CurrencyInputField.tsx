import {
    NumberDecrementStepper,
    NumberIncrementStepper,
    NumberInput,
    NumberInputField,
    NumberInputStepper,
} from "@chakra-ui/react";
import { StringOrNumber } from "@chakra-ui/utils";
import React from "react";

interface CurrencyInputFieldProps {
    onChange: any;
    value: StringOrNumber | undefined;
    min?: number;
    max?: number;
}

export default function CurrencyInputField(
    {
        onChange,
        value,
        min,
        max,
    }: CurrencyInputFieldProps,
) {
    let minAmount = min || 0;
    let maxAmount = max || Number.MAX_SAFE_INTEGER;
    return (
        <NumberInput
            onChange={onChange}
            value={value}
            defaultValue={minAmount}
            min={minAmount}
            max={maxAmount}
        >
            <NumberInputField />
            <NumberInputStepper>
                <NumberIncrementStepper />
                <NumberDecrementStepper />
            </NumberInputStepper>
        </NumberInput>
    );
}
