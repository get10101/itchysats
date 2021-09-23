export interface CfdSellOrderPayload {
    price: number;
    min_quantity: number;
    max_quantity: number;
}

export interface AcceptOrderRequestPayload {
    order_id: string;
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
        throw new Error("failed to publish new order");
    }
}

export async function postAcceptOrder(payload: AcceptOrderRequestPayload) {
    let res = await fetch(
        `/api/order/accept`,
        { method: "POST", body: JSON.stringify(payload), credentials: "include" },
    );
}

export async function postRejectOrder(payload: AcceptOrderRequestPayload) {
    let res = await fetch(
        `/api/order/reject`,
        { method: "POST", body: JSON.stringify(payload), credentials: "include" },
    );
}
