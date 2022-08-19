import { useState } from "react";
import { useEventSourceListener } from "react-sse-hooks";

export default function useLatestEvent<T,>(
    source: EventSource,
    event_name: string,
    mapping: (key: string, value: any) => any = (key, value) => value,
): T | null {
    const [state, setState] = useState<T | null>(null);

    useEventSourceListener<T | null>(
        {
            source: source,
            startOnInit: true,
            event: {
                name: event_name,
                listener: ({ event }) => {
                    // @ts-ignore - yes, there is a data field on event
                    const data = JSON.parse(event.data, mapping);
                    setState(data);
                },
            },
        },
        [source, event_name],
    );

    return state;
}
