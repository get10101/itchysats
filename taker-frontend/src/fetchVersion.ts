import axios, { AxiosResponse } from "axios";
import * as semver from "semver";
import { SemVer } from "semver";
import { DaemonVersion, GithubVersion } from "./types";

export async function fetchGithubVersion(setGithubVersion: (version: SemVer | null) => void) {
    try {
        const response: AxiosResponse<GithubVersion> = await axios.get(
            "https://api.github.com/repos/itchysats/itchysats/releases/latest",
        );

        let githubVersion = response.data;

        if (githubVersion?.tag_name) {
            let version = semver.parse(githubVersion?.tag_name);
            if (!version) {
                // needed because 0.4.20.1 is not a valid semver version
                version = semver.coerce(githubVersion?.tag_name);
            }
            setGithubVersion(version);
        }
    } catch (error: any) {
        if (axios.isAxiosError(error)) {
            console.error(error);
        } else {
            // if it is not coming from axios then something else is odd and we just throw it up
            throw error;
        }
        return;
    }
}

export async function fetchDaemonVersion(setDaemonVersion: (version: SemVer | null) => void) {
    const response: AxiosResponse<DaemonVersion> = await axios.get("/api/version");

    let daemonVersion = response.data;
    if (daemonVersion?.daemon_version) {
        let version = semver.parse(daemonVersion?.daemon_version);
        setDaemonVersion(version);
    }
}
