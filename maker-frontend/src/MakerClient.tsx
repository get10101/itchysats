import { HttpError } from "./components/HttpError";

export interface CfdSellOrderPayload {
    price: number;
    price_long?: number;
    min_quantity: number;
    max_quantity: number;
    funding_rate?: number;
    daily_funding_rate_long?: number;
    opening_fee?: number;
}

export async function postCfdSellOrderRequest(payload: CfdSellOrderPayload) {
    let res = await fetch(`/api/order/sell`, {
        method: "POST",
        body: JSON.stringify(payload),
        headers: {
            "Content-Type": "application/json",
        },
        credentials: "include",
    });

    if (!res.status.toString().startsWith("2")) {
        console.log("Status: " + res.status + ", " + res.statusText);
        const resp = await res.json();
        throw new HttpError(resp);
    }
}
