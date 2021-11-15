import { useEffect, useRef } from "react";

async function fetchWithTimeout(resource: any, timeout: number) {
    const controller = new AbortController();
    const id = setTimeout(() => controller.abort(), timeout);
    const response = await fetch(resource, {
        signal: controller.signal,
    });
    clearTimeout(id);
    return response;
}

// Check for backend's presence by sending a request to '/alive' endpoint.
// When the backend's not there, the request is likely to timeout, triggering a
// persistent toast notification that goes away when the daemon is back online.
// `description` is a user-facing string describing problem/possible action.
export function useBackendMonitor(toast: any, timeout_ms: number, description: string): void {
    const toastIdRef = useRef();

    const checkForBackend: () => void = async () => {
        try {
            const res = await fetchWithTimeout("/alive", timeout_ms);
            // In case we get a response, but it's not what we expected
            if (res.status.toString().startsWith("2")) {
                if (toastIdRef.current) {
                    toast.close(toastIdRef.current);
                }
            } else {
                throw new Error(res.statusText);
            }
        } catch (e) {
            if (!toastIdRef.current) {
                toastIdRef.current = toast({
                    title: "Connection Error",
                    description,
                    status: "error",
                    position: "top",
                    duration: timeout_ms * 100000, // we don't want this to be closed
                    isClosable: false,
                });
            }
        }
    };

    useEffect(() => {
        const interval = setInterval(() => checkForBackend(), timeout_ms);
        return () => {
            clearInterval(interval);
        };
    });
}
