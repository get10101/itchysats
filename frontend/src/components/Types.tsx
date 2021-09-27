export interface Position {
    label: string;
    colorScheme: string;
}

export interface Order {
    id: string;
    trading_pair: string;
    position: Position;
    price: number;
    min_quantity: number;
    max_quantity: number;
    leverage: number;
    liquidation_price: number;
    creation_timestamp: number;
    term_in_secs: number;
}

export interface Cfd {
    order_id: string;
    initial_price: number;

    leverage: number;
    trading_pair: string;
    position: Position;
    liquidation_price: number;

    quantity_usd: number;

    margin: number;

    profit_btc: number;
    profit_usd: number;

    state: State;
    state_transition_timestamp: number;
}

export interface State {
    label: string;
    meta_state: string;
    colorScheme: string;
}

export interface WalletInfo {
    balance: number;
    address: string;
    last_updated_at: number;
}

export function unixTimestampToDate(unixTimestamp: number): Date {
    return new Date(unixTimestamp * 1000);
}
