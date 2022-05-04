import { Tooltip } from "@chakra-ui/react";
import * as React from "react";
import { PropsWithChildren } from "react";

type InterestRateTooltipProps = PropsWithChildren<{
    interestRateHourly?: number;
    interestRateAnnualized?: number;
    disabled: boolean;
}>;

export function InterestRateTooltip(
    { children, interestRateHourly, interestRateAnnualized, disabled }: InterestRateTooltipProps,
) {
    let label = `While your position is open you will pay hourly ${Math.abs(interestRateHourly || 0)}% (${
        Math.abs(interestRateAnnualized || 0)
    }% p.a.).
        This amount will fluctuate depending on the market movement and can also become positive.`;
    if (interestRateAnnualized && interestRateAnnualized > 0) {
        label =
            `While your position is open you will receive hourly ${interestRateHourly}% (${interestRateAnnualized}% p.a.).
        This amount will fluctuate depending on the market movement and can also become negative.`;
    }
    return (
        <Tooltip
            label={label}
            hasArrow
            placement={"right"}
            isDisabled={disabled}
        >
            {children}
        </Tooltip>
    );
}
