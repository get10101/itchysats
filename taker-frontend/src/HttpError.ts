// A wrapper to parse RFC 7807
// Pass result of `await response.json()` into the constructor.
export default class HttpError extends Error {
    title: string;
    detail?: string;

    // FIXME: Constructor can't be async, so we can't pass `Response` here
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
