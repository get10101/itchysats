function quantoLiquidationPrice(leverage: number, quantity: number, price: number) {
    return (price * (1 / leverage));
}

// returns the liquidation price for quanto contracts e.g. ETHUSD
export function liquidationPriceQuantoContracts(leverage: number, quantity: number, price: number, isLong: boolean) {
    if (isLong) {
        const liquidationPrice = price - quantoLiquidationPrice(leverage, quantity, price);
        if (liquidationPrice <= 0) {
            return 1;
        }
        return liquidationPrice;
    } else {
        return price + quantoLiquidationPrice(leverage, quantity, price);
    }
}

// returns the liquidation price for inverse contracts e.g. BTCUSD rounded to 0
export function liquidationPriceInverseContracts(leverage: number, price: number, isLong: boolean) {
    if (isLong) {
        return Math.round(price * leverage / (leverage + 1));
    } else {
        if (leverage === 1) {
            // With a leverage of 1 there is theoretically no liquidation
            return 21_000_000;
        }
        return Math.round(price * leverage / (leverage - 1));
    }
}
