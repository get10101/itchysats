import "@testing-library/jest-dom/extend-expect";
import { liquidationPriceInverseContracts, liquidationPriceQuantoContracts } from "./LiquidationPrice";

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

it("given inverse contract with leverage 1 compute short liquidation price", () => {
    let liquidationPrice = liquidationPriceInverseContracts(1, 1000, true);
    expect(liquidationPrice).toBe(500);
});

it("given inverse contract with leverage 1 compute long liquidation price", () => {
    let liquidationPrice = liquidationPriceInverseContracts(1, 1000, false);
    expect(liquidationPrice).toBe(21_000_000);
});

it("given inverse contract with leverage 2 compute long liquidation price", () => {
    let liquidationPrice = liquidationPriceInverseContracts(2, 60000, true);
    expect(liquidationPrice).toBe(40_000);
});

it("given inverse contract with leverage 2 compute short liquidation price", () => {
    let liquidationPrice = liquidationPriceInverseContracts(2, 60000, false);
    expect(liquidationPrice).toBe(120_000);
});
