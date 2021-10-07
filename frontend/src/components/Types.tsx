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

export class Position {
    constructor(public key: PositionKey) {}

    public getColorScheme(): string {
        switch (this.key) {
            case PositionKey.BUY:
                return "green";
            case PositionKey.SELL:
                return "blue";
        }
    }
}

enum PositionKey {
    BUY = "Buy",
    SELL = "Sell",
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
    profit_in_percent: number;

    state: State;
    actions: Action[];
    state_transition_timestamp: number;
}

export class State {
    constructor(public key: StateKey) {}

    public getLabel(): string {
        switch (this.key) {
            case StateKey.INCOMING_ORDER_REQUEST:
                return "Order Requested";
            case StateKey.OUTGOING_ORDER_REQUEST:
                return "Order Requested";
            case StateKey.ACCEPTED:
                return "Accepted";
            case StateKey.REJECTED:
                return "Rejected";
            case StateKey.CONTRACT_SETUP:
                return "Contract Setup";
            case StateKey.PENDING_OPEN:
                return "Pending Open";
            case StateKey.OPEN:
                return "Open";
            case StateKey.PENDING_COMMIT:
                return "Pending Commit";
            case StateKey.OPEN_COMMITTED:
                return "Open (commit-tx published)";
            case StateKey.INCOMING_SETTLEMENT_PROPOSAL:
                return "Settlement Proposed";
            case StateKey.OUTGOING_SETTLEMENT_PROPOSAL:
                return "Settlement Proposed";
            case StateKey.INCOMING_ROLL_OVER_PROPOSAL:
                return "Rollover Proposed";
            case StateKey.OUTGOING_ROLL_OVER_PROPOSAL:
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
            case StateKey.ACCEPTED:
            case StateKey.OPEN:
                return green;

            case StateKey.REJECTED:
                return red;

            case StateKey.PENDING_COMMIT:
            case StateKey.OPEN_COMMITTED:
            case StateKey.MUST_REFUND:
            case StateKey.PENDING_CET:
                return orange;

            case StateKey.OUTGOING_ORDER_REQUEST:
            case StateKey.INCOMING_ORDER_REQUEST:
            case StateKey.OUTGOING_SETTLEMENT_PROPOSAL:
            case StateKey.INCOMING_SETTLEMENT_PROPOSAL:
            case StateKey.INCOMING_ROLL_OVER_PROPOSAL:
            case StateKey.OUTGOING_ROLL_OVER_PROPOSAL:
            case StateKey.CONTRACT_SETUP:
            case StateKey.PENDING_OPEN:
            case StateKey.REFUNDED:
            case StateKey.SETUP_FAILED:
            case StateKey.CLOSED:
                return default_color;
        }
    }

    public getGroup(): StateGroupKey {
        switch (this.key) {
            case StateKey.INCOMING_ORDER_REQUEST:
                return StateGroupKey.ACCEPT_OR_REJECT_ORDER;

            case StateKey.OUTGOING_ORDER_REQUEST:
            case StateKey.ACCEPTED:
            case StateKey.CONTRACT_SETUP:
                return StateGroupKey.OPENING;

            case StateKey.PENDING_OPEN:
            case StateKey.OPEN:
            case StateKey.PENDING_COMMIT:
            case StateKey.OPEN_COMMITTED:
            case StateKey.MUST_REFUND:
            case StateKey.OUTGOING_SETTLEMENT_PROPOSAL:
            case StateKey.OUTGOING_ROLL_OVER_PROPOSAL:
            case StateKey.PENDING_CET:
                return StateGroupKey.OPEN;

            case StateKey.INCOMING_SETTLEMENT_PROPOSAL:
                return StateGroupKey.ACCEPT_OR_REJECT_SETTLEMENT;

            case StateKey.INCOMING_ROLL_OVER_PROPOSAL:
                return StateGroupKey.ACCEPT_OR_REJECT_ROLL_OVER;

            case StateKey.REJECTED:
            case StateKey.REFUNDED:
            case StateKey.SETUP_FAILED:
            case StateKey.CLOSED:
                return StateGroupKey.CLOSED;
        }
    }
}

export enum Action {
    ACCEPT_ORDER = "acceptOrder",
    REJECT_ORDER = "rejectOrder",
    COMMIT = "commit",
    SETTLE = "settle",
    ROLL_OVER = "rollOver",
    ACCEPT_SETTLEMENT = "acceptSettlement",
    REJECT_SETTLEMENT = "rejectSettlement",
    ACCEPT_ROLL_OVER = "acceptRollOver",
    REJECT_ROLL_OVER = "rejectRollOver",
}

const enum StateKey {
    OUTGOING_ORDER_REQUEST = "OutgoingOrderRequest",
    INCOMING_ORDER_REQUEST = "IncomingOrderRequest",
    ACCEPTED = "Accepted",
    REJECTED = "Rejected",
    CONTRACT_SETUP = "ContractSetup",
    PENDING_OPEN = "PendingOpen",
    OPEN = "Open",
    PENDING_COMMIT = "PendingCommit",
    PENDING_CET = "PendingCet",
    OPEN_COMMITTED = "OpenCommitted",
    OUTGOING_SETTLEMENT_PROPOSAL = "OutgoingSettlementProposal",
    INCOMING_SETTLEMENT_PROPOSAL = "IncomingSettlementProposal",
    OUTGOING_ROLL_OVER_PROPOSAL = "OutgoingRollOverProposal",
    INCOMING_ROLL_OVER_PROPOSAL = "IncomingRollOverProposal",
    MUST_REFUND = "MustRefund",
    REFUNDED = "Refunded",
    SETUP_FAILED = "SetupFailed",
    CLOSED = "Closed",
}

export enum StateGroupKey {
    /// A CFD which is still being set up (not on chain yet)
    OPENING = "Opening",
    ACCEPT_OR_REJECT_ORDER = "Accept / Reject Order",
    /// A CFD that is an ongoing open position (on chain)
    OPEN = "Open",
    ACCEPT_OR_REJECT_SETTLEMENT = "Accept / Reject Settlement",
    ACCEPT_OR_REJECT_ROLL_OVER = "Accept / Reject Roll Over",
    /// A CFD that has been successfully or not-successfully terminated
    CLOSED = "Closed",
}

export interface WalletInfo {
    balance: number;
    address: string;
    last_updated_at: number;
}

export interface PriceInfo {
    bid: number;
    ask: number;
    last_updated_at: number;
}

export function unixTimestampToDate(unixTimestamp: number): Date {
    return new Date(unixTimestamp * 1000);
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

export function intoOrder(key: string, value: any): any {
    switch (key) {
        case "position":
            return new Position(value);
        default:
            return value;
    }
}
