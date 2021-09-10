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
}

export default function CurrencyInputField(
    {
        onChange,
        value,
    }: CurrencyInputFieldProps,
) {
    return (
        <NumberInput
            onChange={onChange}
            value={value}
        >
            <NumberInputField />
            <NumberInputStepper>
                <NumberIncrementStepper />
                <NumberDecrementStepper />
            </NumberInputStepper>
        </NumberInput>
    );
}
