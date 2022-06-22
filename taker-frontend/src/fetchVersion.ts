import * as semver from "semver";
import { SemVer } from "semver";
import { DaemonVersion, GithubVersion } from "./types";

export async function fetchGithubVersion(setGithubVersion: (version: SemVer | null) => void) {
    let response = await fetch("https://api.github.com/repos/itchysats/itchysats/releases/latest", {
        headers: { accept: "application/json" },
    });

    let githubVersion = await response.json() as GithubVersion;
    if (githubVersion?.tag_name) {
        let version = semver.parse(githubVersion?.tag_name);
        if (!version) {
            // needed because 0.4.20.1 is not a valid semver version
            version = semver.coerce(githubVersion?.tag_name);
        }
        setGithubVersion(version);
    }
}

export async function fetchDaemonVersion(setDaemonVersion: (version: SemVer | null) => void) {
    let response = await fetch("/api/version", {
        credentials: "include",
        headers: { accept: "application/json" },
        cache: "default",
    });

    let daemonVersion = await response.json() as DaemonVersion;
    if (daemonVersion?.daemon_version) {
        let version = semver.parse(daemonVersion?.daemon_version);
        setDaemonVersion(version);
    }
}
