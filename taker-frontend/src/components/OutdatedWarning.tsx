import { Alert, AlertDescription, AlertIcon, AlertTitle, CloseButton, Spacer } from "@chakra-ui/react";
import * as React from "react";
import { SemVer } from "semver";

type OutdatedWarningProps = {
    githubVersion: SemVer | undefined | null;
    daemonVersion: SemVer | undefined | null;
    onClose: () => void;
};

export default function OutdatedWarning({ githubVersion, daemonVersion, onClose }: OutdatedWarningProps) {
    return (
        <Alert status="info" position={"sticky"} top={0} zIndex={100}>
            <AlertIcon />
            <AlertTitle>Your daemon is outdated!</AlertTitle>
            <AlertDescription>
                Upgrade now to get the best ItchySats experience. The latest version is '{githubVersion
                    ?.version}' but your version is '{daemonVersion?.version}'.
            </AlertDescription>
            <Spacer />
            <CloseButton alignSelf="flex-start" position="relative" right={-1} top={-1} onClick={onClose} />
        </Alert>
    );
}
