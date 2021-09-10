export interface RustDuration {
    secs: number;
    nanos: number;
}

export interface RustTimestamp {
    secs_since_epoch: number;
    nanos_since_epoch: number;
}

export interface Offer {
    id: string;
    trading_pair: string;
    position: string;
    price: number;
    min_quantity: number;
    max_quantity: number;
    leverage: number;
    liquidation_price: number;
    creation_timestamp: RustTimestamp;
    term: RustDuration;
}

export interface CfdStateCommon {
    transition_timestamp: RustTimestamp;
}

export interface CfdStatePayload {
    common: CfdStateCommon;
    settlement_timestamp?: RustTimestamp; // only in state Open
}

export interface CfdState {
    type: string;
    payload: CfdStatePayload;
}

export interface Cfd {
    offer_id: string;
    initial_price: number;

    leverage: number;
    trading_pair: string;
    position: string;
    liquidation_price: number;

    quantity_usd: number;
    profit_btc: number;
    profit_usd: number;

    state: CfdState;
}
