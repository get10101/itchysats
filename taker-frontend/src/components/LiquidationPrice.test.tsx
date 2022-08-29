import "@testing-library/jest-dom/extend-expect";
import { liquidationPriceQuantoContracts } from "./LiquidationPrice";

it("given quanto contract with leverage 1 compute long liquidation price", () => {
    let liquidationPrice = liquidationPriceQuantoContracts(1, 10, 1000, true);
    expect(liquidationPrice).toBe(1);
});

it("given quanto contract with leverage 1 compute short liquidation price", () => {
    let liquidationPrice = liquidationPriceQuantoContracts(1, 10, 1000, false);
    expect(liquidationPrice).toBe(2000);
});

it("given quanto contract with leverage 2 compute long liquidation price", () => {
    let liquidationPrice = liquidationPriceQuantoContracts(2, 10, 1000, true);
    expect(liquidationPrice).toBe(500);
});

it("given quanto contract with leverage 2 compute short liquidation price", () => {
    let liquidationPrice = liquidationPriceQuantoContracts(2, 10, 1000, false);
    expect(liquidationPrice).toBe(1500);
});
