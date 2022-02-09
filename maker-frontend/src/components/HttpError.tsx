// A wrapper to parse RFC 7807
// Pass result of `await response.json()` into the constructor.
export class HttpError extends Error {
    title: string;
    detail?: string;

    constructor(json_resp: any) {
        let title = json_resp.title;
        super(title);
        this.title = title;
        if (json_resp.detail) {
            this.detail = json_resp.detail;
        }

        Object.setPrototypeOf(this, HttpError.prototype);
    }
}
