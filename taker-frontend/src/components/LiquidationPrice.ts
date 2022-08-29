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
