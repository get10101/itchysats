import { Box } from "@chakra-ui/react";
import { OnChangeValue, Select } from "chakra-react-select";
import React from "react";
import { Symbol } from "../App";

export interface SymbolSelectorProps {
    current: Symbol;
    onChange: (val: string) => void;
}

interface SelectOption {
    value: String;
    label: String;
}

export const SymbolSelector = ({ current, onChange }: SymbolSelectorProps) => {
    let btcUsd = Symbol.btcusd;
    let ethUsd = Symbol.ethusd;

    const onChangeInner = (value: OnChangeValue<SelectOption, boolean>) => {
        if (value) {
            // @ts-ignore: the field `value` exists but the linter complains
            onChange(value.value);
        }
    };
    const options = [
        { value: btcUsd, label: btcUsd.toUpperCase() },
        { value: ethUsd, label: ethUsd.toUpperCase() },
    ];

    return (
        <Box width={"100%"}>
            <Select
                defaultValue={options[0]}
                value={{
                    value: current,
                    label: current.toUpperCase(),
                }}
                selectedOptionColor="orange"
                selectedOptionStyle="color"
                options={options}
                onChange={(item) => onChangeInner(item)}
            />
        </Box>
    );
};
