import { useToast } from "@chakra-ui/react";
import { useAsync } from "react-async";

/**
  * A React hook for sending a POST request to a certain endpoint.
  *
  * You can pass a callback (`onSuccess`) to process the response. By default, we extract the HTTP body as JSON
  */
export default function usePostRequest<Req = any, Res = any>(
    url: string,
    onSuccess: (response: Res) => void = () => {},
): [(req: Req) => void, boolean] {
    const toast = useToast();

    let { run, isLoading } = useAsync({
        deferFn: async ([payload]: any[]) => {
            let res = await fetch(url, {
                method: "POST",
                body: JSON.stringify(payload),
                headers: {
                    "Content-type": "application/json",
                },
                credentials: "include",
            });

            if (!res.status.toString().startsWith("2")) {
                let problem = await res.json() as Problem;

                toast({
                    title: "Error: " + problem.title,
                    description: problem.detail,
                    status: "error",
                    duration: 10000,
                    isClosable: true,
                });

                return;
            }

            let responseType = res.headers.get("Content-type");

            if (responseType && responseType.startsWith("application/json")) {
                onSuccess(await res.json() as Res);
                return;
            }

            if (responseType && responseType.startsWith("text/plain")) {
                onSuccess(await res.text() as unknown as Res); // `unknown` cast is not ideal because we known that `.text()` gives us string.
                return;
            }

            // if none of the above content types match, pass bytes to the caller
            onSuccess(await res.blob() as unknown as Res); // `unknown` cast is not ideal because we known that `.blob()` gives us as blob.
        },
    });

    return [run as (req: Req) => void, isLoading];
}

interface Problem {
    title: string;
    detail: string;
}
