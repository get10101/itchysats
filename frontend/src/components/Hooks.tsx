import React, { useState } from "react";
import { useEventSource, useEventSourceListener } from "react-sse-hooks";

export default function useLatestEvent<T>(source: EventSource, event_name: string): T | null {
    const [state, setState] = useState<T | null>(null);

    useEventSourceListener<T | null>(
        {
            source: source,
            startOnInit: true,
            event: {
                name: event_name,
                listener: ({ data }) => setState(data),
            },
        },
        [source],
    );

    return state;
}
