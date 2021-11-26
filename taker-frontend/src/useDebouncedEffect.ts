import { useEffect } from "react";

// Copied from: https://stackoverflow.com/a/61127960/2489334
export default function useDebouncedEffect(effect: () => void, deps: any[] | undefined, delay: number) {
    return useEffect(() => {
        const handler = setTimeout(() => effect(), delay);

        return () => clearTimeout(handler);
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [...deps || [], delay]);
}
