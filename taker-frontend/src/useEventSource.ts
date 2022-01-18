import { useEffect, useState } from "react";

export function useEventSource(url: string): [EventSource | null, boolean] {
    const [source, setSource] = useState<EventSource | null>(null);
    const [isConnected, setIsConnected] = useState<boolean>(true);

    // Construct a new event source if the arguments to this hook change
    useEffect(() => {
        const es = new EventSource(url, { withCredentials: true });
        setSource(es);

        const errorHandler = () => {
            setIsConnected(false);
            setSource(null);
        };

        es.addEventListener("error", errorHandler);

        let timer = setTimeout(() => {
            setIsConnected(false);
        }, 10000);

        const heartbeatHandler = (event: Event) => {
            clearTimeout(timer);
            setIsConnected(true);

            const hearbeat: Heartbeat = JSON.parse((event as EventSourceEvent).data);
            const interval_msecs = hearbeat.interval * 1000;
            const buffered_interval_msecs = interval_msecs * 2;
            timer = setTimeout(() => {
                setIsConnected(false);
            }, buffered_interval_msecs);
        };

        es.addEventListener("heartbeat", heartbeatHandler);

        return () => {
            setSource(null);
            es.removeEventListener("error", errorHandler);
            es.removeEventListener("heartbeat", heartbeatHandler);
            clearTimeout(timer);
            es.close();
        };
    }, [url]);

    return [source, isConnected];
}

type EventSourceEvent = Event & { data: string };
interface Heartbeat {
    timestamp: number;
    interval: number;
}
