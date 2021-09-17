import { Plugin } from "vite";

export default function dynamicApp(app: string): Plugin {
    return {
        name: "dynamicApp", // required, will show up in warnings and errors
        resolveId: (id) => {
            // For some reason these are different?
            const productionBuildId = "./__app__.tsx";
            const devBuildId = "/__app__.tsx";

            if (id === productionBuildId || id === devBuildId) {
                return `${__dirname}/${app}.tsx`;
            }

            return null;
        },
    };
}
