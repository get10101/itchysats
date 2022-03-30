import { Tooltip } from "@chakra-ui/react";
import * as React from "react";
import { PropsWithChildren } from "react";

type FundingRateTooltipProps = PropsWithChildren<{
    fundingRateHourly?: number;
    fundingRateAnnualized?: number;
    disabled: boolean;
}>;

export function FundingRateTooltip(
    { children, fundingRateHourly, fundingRateAnnualized, disabled }: FundingRateTooltipProps,
) {
    return (
        <Tooltip
            label={`The CFD is rolled over perpetually every hour at ${fundingRateHourly}% (${fundingRateAnnualized}% p.a.). 
        The cost can fluctuate depending on the market movements.`}
            hasArrow
            placement={"right"}
            isDisabled={disabled}
        >
            {children}
        </Tooltip>
    );
}
