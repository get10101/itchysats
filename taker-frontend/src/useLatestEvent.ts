import { useEffect, useState } from "react";

export default function useLatestEvent<T>(
    source: EventSource | null,
    eventName: string,
    mapping: (key: string, value: any) => any = (_, value) => value,
    filter?: (data: any) => boolean,
): T | null {
    const [state, setState] = useState<T | null>(null);

    useEffect(() => {
        if (source) {
            const listener = (event: Event) => {
                const data = JSON.parse((event as EventSourceEvent).data, mapping);
                if (filter !== undefined && !filter(data)) {
                    return;
                }
                setState(data);
            };

            source.addEventListener(eventName, listener);
            return () => source.removeEventListener(eventName, listener);
        }
        return undefined;
    }, [source, eventName, mapping, filter]);

    return state;
}

export type EventSourceEvent = Event & { data: string };
