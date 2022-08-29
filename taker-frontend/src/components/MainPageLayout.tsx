import { Box, Center, useColorModeValue } from "@chakra-ui/react";
import * as React from "react";
import { Outlet } from "react-router-dom";
import { SemVer } from "semver";
import { BG_DARK, BG_LIGHT, FOOTER_HEIGHT, HEADER_HEIGHT, VIEWPORT_WIDTH, VIEWPORT_WIDTH_PX } from "../App";
import { ConnectionStatus, IdentityInfo } from "../types";
import Footer from "./Footer";
import Nav from "./NavBar";

type MainPageProps = {
    outdatedWarningIsVisible: boolean;
    incompatibleWarningIsVisible: boolean;
    githubVersion: SemVer | null | undefined;
    daemonVersion: SemVer | null | undefined;
    onCloseOutdatedWarning: () => void;
    onCloseIncompatibleWarning: () => void;
    connectedToMaker: ConnectionStatus;
    nextFundingEvent: string | null;
    referencePrice: number | undefined;
    identityOrUndefined: IdentityInfo | null;
    setExtraInfo: (val: boolean) => void;
    showExtraInfo: boolean;
};

export function MainPageLayout(
    {
        outdatedWarningIsVisible,
        incompatibleWarningIsVisible,
        githubVersion,
        daemonVersion,
        onCloseOutdatedWarning,
        onCloseIncompatibleWarning,
        connectedToMaker,
        nextFundingEvent,
        referencePrice,
        identityOrUndefined,
        setExtraInfo,
        showExtraInfo,
    }: MainPageProps,
) {
    return (
        <>
            <Nav
                connectedToMaker={connectedToMaker}
                nextFundingEvent={nextFundingEvent}
                referencePrice={referencePrice}
                daemonVersion={daemonVersion}
                githubVersion={githubVersion}
                onCloseOutdatedWarning={onCloseOutdatedWarning}
                outdatedWarningIsVisible={outdatedWarningIsVisible}
                incompatibleWarningIsVisible={incompatibleWarningIsVisible}
                onCloseIncompatibleWarning={onCloseIncompatibleWarning}
            >
                <Center>
                    <Box
                        maxWidth={(VIEWPORT_WIDTH + 200) + "px"}
                        width={"100%"}
                    >
                        <Center>
                            <Box
                                textAlign="center"
                                bg={useColorModeValue(BG_LIGHT, BG_DARK)}
                                maxWidth={VIEWPORT_WIDTH_PX}
                                marginTop={(HEADER_HEIGHT
                                    + (outdatedWarningIsVisible || incompatibleWarningIsVisible
                                        ? HEADER_HEIGHT
                                        : 0)) + "px"}
                                minHeight={`calc(100vh - ${FOOTER_HEIGHT}px - ${HEADER_HEIGHT}px)`}
                                width={"100%"}
                            >
                                <Outlet />
                            </Box>
                        </Center>
                    </Box>
                </Center>

                <Footer
                    identityInfo={identityOrUndefined}
                    daemonVersion={daemonVersion?.version}
                    githubVersion={githubVersion?.version}
                    onExtraInfoToggle={setExtraInfo}
                    showExtraInfo={showExtraInfo}
                />
            </Nav>
        </>
    );
}
