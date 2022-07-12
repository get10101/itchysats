import { useToast } from "@chakra-ui/react";
import { useAsync } from "@react-hookz/web";
import axios, { AxiosResponse } from "axios";

/**
 * A React hook for sending a POST request to a certain endpoint.
 *
 * You can pass a callback (`onSuccess`) to process the response. By default, we extract the HTTP body as JSON
 */
export default function usePostRequest<Req = any, Res = any>(
    url: string,
    onSuccess: (response: Res) => void = () => {},
): [([req]: [Req]) => void, boolean] {
    const toast = useToast();

    let [{ status }, { execute }] = useAsync(
        async ([payload]: any[]) => {
            try {
                let res: AxiosResponse<Res> = await axios.post(url, payload, {
                    headers: {
                        "Content-type": "application/json",
                    },
                    withCredentials: true,
                });

                onSuccess(res.data);
            } catch (error: any) {
                if (axios.isAxiosError(error)) {
                    let problem = error.toJSON() as Problem;
                    toast({
                        title: "Error: " + problem.title,
                        description: problem.detail,
                        status: "error",
                        duration: 10000,
                        isClosable: true,
                    });
                } else {
                    // if it is not coming from axios then something else is odd and we just throw it up
                    throw error;
                }
                return;
            }
        },
    );
    return [execute, status === "loading"];
}

interface Problem {
    title: string;
    detail: string;
}
