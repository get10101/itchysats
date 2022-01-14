export interface WalletInfo {
    balance: number;
    address: string;
    last_updated_at: number;
}

export function unixTimestampToDate(unixTimestamp: number): Date {
    return new Date(unixTimestamp * 1000);
}

export interface Order {
    id: string;
    trading_pair: string;
    position: Position;
    price: string;
    min_quantity: string;
    max_quantity: string;
    parcel_size: string;
    margin_per_parcel: number;
    leverage: number;
    liquidation_price: string;
    creation_timestamp: number;
    settlement_time_interval_in_secs: number;
}

export class Position {
    constructor(public key: PositionKey) {}

    public getColorScheme(): string {
        switch (this.key) {
            case PositionKey.LONG:
                return "green";
            case PositionKey.SHORT:
                return "blue";
        }
    }
}

enum PositionKey {
    LONG = "Long",
    SHORT = "Short",
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

    profit_btc?: number;
    profit_percent?: number;

    state: State;
    state_transition_timestamp: number;
    details: CfdDetails;
    expiry_timestamp?: number;

    counterparty: string;
}

export interface CfdDetails {
    tx_url_list: Tx[];
    payout?: number;
}

export interface Tx {
    label: TxLabel;
    url: string;
}

export enum TxLabel {
    Lock = "Lock",
    Commit = "Commit",
    Cet = "Cet",
    Refund = "Refund",
    Collaborative = "Collaborative",
}

export class State {
    constructor(public key: StateKey) {}

    public getLabel(): string {
        switch (this.key) {
            case StateKey.PENDING_SETUP:
                return "Offered";
            case StateKey.CONTRACT_SETUP:
                return "Contract Setup";
            case StateKey.REJECTED:
                return "Rejected";
            case StateKey.PENDING_OPEN:
                return "Pending Open";
            case StateKey.OPEN:
                return "Open";
            case StateKey.PENDING_COMMIT:
                return "Pending Commit";
            case StateKey.PENDING_CLOSE:
                return "Pending Close";
            case StateKey.OPEN_COMMITTED:
                return "Open (commit-tx published)";
            case StateKey.INCOMING_SETTLEMENT_PROPOSAL:
                return "Settlement Proposed";
            case StateKey.OUTGOING_SETTLEMENT_PROPOSAL:
                return "Settlement Proposed";
            case StateKey.INCOMING_ROLLOVER_PROPOSAL:
                return "Rollover Proposed";
            case StateKey.OUTGOING_ROLLOVER_PROPOSAL:
                return "Rollover Proposed";
            case StateKey.MUST_REFUND:
                return "Refunding";
            case StateKey.REFUNDED:
                return "Refunded";
            case StateKey.SETUP_FAILED:
                return "Setup Failed";
            case StateKey.PENDING_CET:
                return "Pending CET";
            case StateKey.CLOSED:
                return "Closed";
        }
    }

    public getColorScheme(): string {
        const default_color = "gray";
        const green = "green";
        const red = "red";
        const orange = "orange";

        switch (this.key) {
            case StateKey.OPEN:
                return green;

            case StateKey.REJECTED:
                return red;

            case StateKey.PENDING_COMMIT:
            case StateKey.OPEN_COMMITTED:
            case StateKey.MUST_REFUND:
            case StateKey.PENDING_CET:
            case StateKey.PENDING_CLOSE:
                return orange;

            case StateKey.PENDING_SETUP:
            case StateKey.CONTRACT_SETUP:
            case StateKey.OUTGOING_SETTLEMENT_PROPOSAL:
            case StateKey.INCOMING_SETTLEMENT_PROPOSAL:
            case StateKey.INCOMING_ROLLOVER_PROPOSAL:
            case StateKey.OUTGOING_ROLLOVER_PROPOSAL:
            case StateKey.PENDING_OPEN:
            case StateKey.REFUNDED:
            case StateKey.SETUP_FAILED:
            case StateKey.CLOSED:
                return default_color;
        }
    }

    public getGroup(): StateGroupKey {
        switch (this.key) {
            case StateKey.PENDING_SETUP:
            case StateKey.CONTRACT_SETUP:
                return StateGroupKey.OPENING;

            case StateKey.PENDING_OPEN:
            case StateKey.OPEN:
            case StateKey.PENDING_COMMIT:
            case StateKey.OPEN_COMMITTED:
            case StateKey.MUST_REFUND:
            case StateKey.OUTGOING_SETTLEMENT_PROPOSAL:
            case StateKey.OUTGOING_ROLLOVER_PROPOSAL:
            case StateKey.PENDING_CET:
            case StateKey.PENDING_CLOSE:
                return StateGroupKey.OPEN;

            case StateKey.INCOMING_SETTLEMENT_PROPOSAL:
                return StateGroupKey.PENDING_SETTLEMENT;

            case StateKey.INCOMING_ROLLOVER_PROPOSAL:
                return StateGroupKey.PENDING_ROLLOVER;

            case StateKey.REJECTED:
            case StateKey.REFUNDED:
            case StateKey.SETUP_FAILED:
            case StateKey.CLOSED:
                return StateGroupKey.CLOSED;
        }
    }
}

export const enum StateKey {
    PENDING_SETUP = "PendingSetup",
    CONTRACT_SETUP = "ContractSetup",
    REJECTED = "Rejected",
    PENDING_OPEN = "PendingOpen",
    OPEN = "Open",
    PENDING_CLOSE = "PendingClose",
    PENDING_COMMIT = "PendingCommit",
    PENDING_CET = "PendingCet",
    OPEN_COMMITTED = "OpenCommitted",
    OUTGOING_SETTLEMENT_PROPOSAL = "OutgoingSettlementProposal",
    INCOMING_SETTLEMENT_PROPOSAL = "IncomingSettlementProposal",
    OUTGOING_ROLLOVER_PROPOSAL = "OutgoingRolloverProposal",
    INCOMING_ROLLOVER_PROPOSAL = "IncomingRolloverProposal",
    MUST_REFUND = "MustRefund",
    REFUNDED = "Refunded",
    SETUP_FAILED = "SetupFailed",
    CLOSED = "Closed",
}

export enum StateGroupKey {
    /// A CFD which is still being set up (not on chain yet)
    OPENING = "Opening",
    /// A CFD that is an ongoing open position (on chain)
    OPEN = "Open",
    PENDING_SETTLEMENT = "Pending Settlement",
    PENDING_ROLLOVER = "Pending Roll Over",
    /// A CFD that has been successfully or not-successfully terminated
    CLOSED = "Closed",
}

export interface CfdOrderRequestPayload {
    order_id: string;
    quantity: number;
}

export function intoOrder(key: string, value: any): any {
    switch (key) {
        case "position":
            return new Position(value);
        default:
            return value;
    }
}

export function intoCfd(key: string, value: any): any {
    switch (key) {
        case "position":
            return new Position(value);
        case "state":
            return new State(value);
        default:
            return value;
    }
}

export interface BXBTData {
    symbol: string;
    markPrice: number;
    timestamp: string;
}

export interface WithdrawRequest {
    address: string;
    amount?: number;
    fee: number;
}

export interface ConnectionStatus {
    online: boolean;
    connection_close_reason?: ConnectionCloseReason;
}

export const enum ConnectionCloseReason {
    MAKER_VERSION_OUTDATED = "MakerVersionOutdated",
    TAKER_VERSION_OUTDATED = "TakerVersionOutdated",
}
