export interface Offer {
    id: string;
    trading_pair: string;
    position: string;
    price: number;
    min_quantity: number;
    max_quantity: number;
    leverage: number;
    liquidation_price: number;
    creation_unix_timestamp: number;
    term_in_secs: number;
}

export interface Cfd {
    offer_id: string;
    initial_price: number;

    leverage: number;
    trading_pair: string;
    position: string;
    liquidation_price: number;

    quantity_usd: number;

    margin: number;

    profit_btc: number;
    profit_usd: number;

    state: string;
    state_transition_unix_timestamp: number;
}
