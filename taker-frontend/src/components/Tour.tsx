import { ExternalLinkIcon } from "@chakra-ui/icons";
import { Link, Text } from "@chakra-ui/react";
import { Steps } from "intro.js-react";
import * as React from "react";
import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { FAQ_URL } from "../App";
import cfd101 from "../images/CFD_101_light_bg.svg";
import confirmationDialog from "../images/confirmation_dialog.png";
import itchyGuyWelcome from "../images/ItchyGuyWelcome.svg";

export const Tour = () => {
    const tourSteps = [
        {
            title: "G'Day Satoshi!",
            intro: (
                <>
                    <img src={itchyGuyWelcome} width={"400px"} alt={"Mr Ichty Sats, Bitcoin-lover"} />
                    <Text>
                        Before we start please be advised that ItchySats is still under heavy development and was not
                        fully audited yet.
                        <br />
                        <br />
                        Additionally, CFD trading is inherently risky, make sure to read up so you don't get rekt.
                        <br />
                        <br />
                        Follow the tour for a basic intro, for more questions check the&nbsp;<FaqLink />.
                    </Text>
                </>
            ),
            position: "right",
        },
        {
            element: "#walletSwitchButton",
            intro:
                "Before you can open a position you will have to add funds to your wallet. We will come back to this at the end of the tour.",
            position: "right",
        },
        {
            title: "Long and Short",
            intro: (
                <>
                    <Text>
                        When opening a long position you make profits if the Bitcoin price goes up.
                        <br />
                        When opening a short position you make profits if the Bitcoin price goes down.
                        <br />
                        <img src={cfd101} width={"400px"} alt={"CFD 101"} />
                    </Text>
                </>
            ),
            element: "#longShortButtonSwitch",
            position: "right",
        },
        {
            element: "#makerLongPrice",
            intro: "This is the current price of the maker for opening a long position...",
            position: "right",
        },
        {
            element: "#longQuantityInput",
            intro:
                "You specify how many contracts of BTC/USD you buy. This will determine the margin that will be locked up on chain.",
            position: "right",
        },
        {
            element: "#longLeverage",
            // TODO: Add link to learn about leverage trading
            intro: "The leverage influences the margin as well. At the moment the leverage is fixed at x2.",
            position: "right",
        },
        {
            element: "#longRequiredMargin",
            intro:
                "This is the amount of BTC that is necessary to open the position. This amount will be locked on chain.",
            position: "right",
        },
        {
            element: "#longPerpetualCost",
            intro:
                "To allow you to close any point in time in the future you pay a small fee per the hour. Initially you will pay fees for 24h, with every hour that the CFD remains open you pay for one additional hour.",
            position: "right",
        },
        {
            element: "#longButton",
            intro: (
                <>
                    <Text>
                        Clicking this button will open a confirmation dialog similar to this:
                        <br />
                        <img src={confirmationDialog} width={"400px"} alt={"Confirm it already!"} />
                        <br />
                        Once you confirm the CFD will be opened with the maker, resulting in your and the maker's margin
                        being locked up on chain.
                        <br />
                        <br />
                        You can close an open position at any point in time.
                    </Text>
                </>
            ),
            position: "right",
        },
        {
            title: "Time for funding the wallet!",
            element: "#walletSwitchButton",
            intro: "Time to add some funds to the wallet... Let me take you there...",
            position: "right",
        },
        {
            title: "Happy trading!",
            intro: (
                <>
                    <Text>
                        On Umbrel the wallet is derived from your Umbrel seed. For details check the&nbsp;<FaqLink />
                        <br />
                        <br />
                        Send BTC to the address below. A new address will be derived after usage. Balance is picked up
                        once the tx is seen in mempool. You can then open a position.
                    </Text>
                </>
            ),
            position: "right",
        },
    ];

    const [tourEnabled, setTourEnabled] = useState(true);

    const onExit = () => {
        setTourEnabled(false);
    };

    const navigate = useNavigate();
    const onChange = (nextStepIndex: number) => {
        // Before the last step we jump to the wallet
        if (nextStepIndex === (tourSteps.length - 1)) {
            navigate("/wallet");
        }
    };

    return (
        <Steps
            enabled={tourEnabled}
            steps={tourSteps}
            initialStep={0}
            onExit={onExit}
            onChange={onChange}
            options={{
                // This option will show a checkbox "don't show this again" checkbox in each step
                // If the user ticks it, a cookie will be set and the user does not see the tour again.
                dontShowAgain: true,
                nextToDone: true,
                keyboardNavigation: true,
                showBullets: true,
                disableInteraction: true,
            }}
        />
    );
};

const FaqLink = () => {
    return (
        <u>
            <Link href={FAQ_URL} isExternal>FAQ&nbsp;</Link>
            <ExternalLinkIcon />
        </u>
    );
};
