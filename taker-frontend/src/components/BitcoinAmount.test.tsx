import "@testing-library/jest-dom/extend-expect";
import { render } from "@testing-library/react";
import { screen } from "@testing-library/react";
import React from "react";
import BitcoinAmount from "./BitcoinAmount";

it("renders amount always with 8 digits", () => {
    render(<BitcoinAmount btc={0.0001} />);

    expect(screen.getByText("₿0.00010000")).toBeInTheDocument();
});

it("renders 1 satoshi without scientific notation", () => {
    render(<BitcoinAmount btc={0.00000001} />);

    expect(screen.getByText("₿0.00000001")).toBeInTheDocument();
});

it("renders less than one satoshi as 0", () => {
    render(<BitcoinAmount btc={0.000000000000001} />);

    expect(screen.getByText("₿0.00000000")).toBeInTheDocument();
});
