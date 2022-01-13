import { useEffect, useState } from "react";

export function useEventSource(url: string, withCredentials?: boolean): [EventSource | null, boolean] {
    const [source, setSource] = useState<EventSource | null>(null);
    const [isConnected, setIsConnected] = useState<boolean>(true);

    // Construct a new event source if the arguments to this hook change
    useEffect(() => {
        const es = new EventSource(url, { withCredentials });
        setSource(es);

        const errorHandler = () => {
            setIsConnected(false);
            setSource(null);
        };

        es.addEventListener("error", errorHandler);

        let timer = setTimeout(() => {
            setIsConnected(false);
        }, 5000);

        const heartbeatHandler = () => {
            clearTimeout(timer);
            setIsConnected(true);
            timer = setTimeout(() => {
                setIsConnected(false);
            }, 5000);
        };

        es.addEventListener("heartbeat", heartbeatHandler);

        return () => {
            setSource(null);
            es.removeEventListener("error", errorHandler);
            es.removeEventListener("heartbeat", heartbeatHandler);
            clearTimeout(timer);
            es.close();
        };
    }, [url, withCredentials]);

    return [source, isConnected];
}
