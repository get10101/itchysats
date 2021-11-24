import HttpError from "../HttpError";

// A generic way of creating an error toast
// TODO: Don't use any (`toast: typeof useToast` did not work :( )
export default function createErrorToast(toast: any, e: any) {
    if (e instanceof HttpError) {
        const description = e.detail ? e.detail : "";

        toast({
            title: "Error: " + e.title,
            description,
            status: "error",
            duration: 10000,
            isClosable: true,
        });
    } else {
        console.log(e);
        const description = typeof e === "string" ? e : JSON.stringify(e);

        toast({
            title: "Error",
            description,
            status: "error",
            duration: 10000,
            isClosable: true,
        });
    }
}
