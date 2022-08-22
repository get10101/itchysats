import { useDisclosure, useToast } from "@chakra-ui/react";
import dayjs from "dayjs";
import isBetween from "dayjs/plugin/isBetween";
import relativeTime from "dayjs/plugin/relativeTime";
import utc from "dayjs/plugin/utc";
import * as React from "react";
import { useEffect, useState } from "react";
import { Route, Routes, useLocation, useNavigate } from "react-router-dom";
import useWebSocket from "react-use-websocket";
import { SemVer } from "semver";
import { MainPageLayout } from "./components/MainPageLayout";
import Trade from "./components/Trade";
import { TradePageLayout } from "./components/TradePageLayout";
import { Wallet } from "./components/Wallet";
import { fetchDaemonVersion, fetchGithubVersion } from "./fetchVersion";
import {
    BXBTData,
    Cfd,
    ConnectionStatus,
    IdentityInfo,
    intoCfd,
    intoMakerOffer,
    LeverageDetails,
    MakerCompatibility,
    MakerOffer,
    WalletInfo,
} from "./types";
import { useEventSource } from "./useEventSource";
import useLatestEvent from "./useLatestEvent";

export interface Offer {
    id?: string;
    price?: number;
    fundingRateAnnualized?: number;
    fundingRateHourly?: number;
    leverageDetails: LeverageDetails[];

    // defaulted for display purposes
    minQuantity: number;
    maxQuantity: number;
    lotSize: number;
}

// TODO: Evaluate moving these globals into the theme to make them accessible through that
export const VIEWPORT_WIDTH = 1000;
export const FOOTER_HEIGHT = 50;
export const HEADER_HEIGHT = 50;
export const VIEWPORT_WIDTH_PX = VIEWPORT_WIDTH + "px";
export const BG_LIGHT = "gray.50";
export const BG_DARK = "gray.800";
export const FAQ_URL = "http://faq.itchysats.network";

export enum Symbol {
    // we use lower case variant names because of react-router using lower-case routes by default and it is easier to match
    btcusd = "btcusd",
    ethusd = "ethusd",
}

export const App = () => {
    const toast = useToast();
    const navigate = useNavigate();
    const location = useLocation();

    // ideally we could be using useParams() here but `App` is on the top level and is not aware of any params yet,
    // hence we parse the location.
    let symbol = Symbol.btcusd;
    if (location.pathname.includes("ethusd")) {
        symbol = Symbol.ethusd;
    }

    let [referencePrice, setReferencePrice] = useState<number>();
    let [showExtraInfo, setExtraInfo] = useState(false);
    const [githubVersion, setGithubVersion] = useState<SemVer | null>();
    const [daemonVersion, setDaemonVersion] = useState<SemVer | null>();

    useWebSocket(bitmexWebSocketURL(symbol), {
        shouldReconnect: () => true,
        onMessage: (message) => {
            const data: BXBTData[] = JSON.parse(message.data).data;
            if (data && data[0]?.markPrice) {
                setReferencePrice(data[0].markPrice);
            }
        },
    });

    useEffect(() => {
        void fetchGithubVersion(setGithubVersion);
        void fetchDaemonVersion(setDaemonVersion);
    }, []);

    const [source, isConnected] = useEventSource(`/api/feed`);
    const walletInfo = useLatestEvent<WalletInfo>(source, "wallet");

    let { long_offer, short_offer } = offer_event_names(symbol);
    const makerLong = useLatestEvent<MakerOffer>(
        source,
        long_offer,
        intoMakerOffer,
    );
    const makerShort = useLatestEvent<MakerOffer>(
        source,
        short_offer,
        intoMakerOffer,
    );

    const identityOrUndefined = useLatestEvent<IdentityInfo>(source, "identity");

    const shortOffer = makerOfferToTakerOffer(makerLong);
    const longOffer = makerOfferToTakerOffer(makerShort);

    function makerOfferToTakerOffer(offer: MakerOffer | null): Offer {
        if (offer) {
            return {
                id: offer.id,
                price: offer.price,
                fundingRateAnnualized: offer.funding_rate_annualized_percent,
                fundingRateHourly: toFixedNumber(offer.funding_rate_hourly_percent, 5),
                minQuantity: offer.min_quantity,
                maxQuantity: offer.max_quantity,
                lotSize: offer.lot_size,
                leverageDetails: offer.leverage_details,
            };
        }

        return {
            leverageDetails: [],
            minQuantity: 0,
            maxQuantity: 0,
            lotSize: 100,
        };
    }

    function toFixedNumber(n: number, digits: number): number {
        // Conversion of the number into Number needed to avoid "toFixed is not a function" errors
        return Number.parseFloat(Number(n).toFixed(digits));
    }

    const cfdsOrUndefined = useLatestEvent<Cfd[]>(source, "cfds", intoCfd);
    let cfds = cfdsOrUndefined ? cfdsOrUndefined! : [];
    const connectedToMakerOrUndefined = useLatestEvent<ConnectionStatus>(source, "maker_status");
    const makerCompatibilityOrUndefined = useLatestEvent<MakerCompatibility>(source, "maker_compatibility");

    let incompatible = false;
    if (makerCompatibilityOrUndefined) {
        incompatible = makerCompatibilityOrUndefined.unsupported_protocols !== null
            && makerCompatibilityOrUndefined.unsupported_protocols !== undefined
            && makerCompatibilityOrUndefined.unsupported_protocols.length > 0;
    }

    const connectedToMaker = connectedToMakerOrUndefined ? connectedToMakerOrUndefined : { online: false };

    dayjs.extend(relativeTime);
    dayjs.extend(utc);
    dayjs.extend(isBetween);

    // TODO: Eventually this should be calculated with what the maker defines in the offer, for now we assume full hour
    const nextFullHour = dayjs().utc().minute(0).add(1, "hour");

    // TODO: this condition is a bit weird now
    const nextFundingEvent = longOffer || shortOffer ? dayjs().to(nextFullHour) : null;

    useEffect(() => {
        const id = "connection-toast";
        if (!isConnected && !toast.isActive(id)) {
            toast({
                id,
                status: "error",
                isClosable: true,
                duration: null,
                position: "bottom",
                title: "Connection error!",
                description: "Please ensure your daemon is running. Then refresh the page.",
            });
        } else if (isConnected && toast.isActive(id)) {
            toast.close(id);
        }
    }, [toast, isConnected]);

    useEffect(() => {
        const id = "maker-connection-toast";
        if (connectedToMakerOrUndefined && !connectedToMakerOrUndefined.online && !toast.isActive(id)) {
            toast({
                id,
                status: "warning",
                isClosable: true,
                duration: null,
                position: "bottom",
                title: "No maker!",
                description: "You are not connected to any maker. Functionality may be limited",
            });
        } else if (connectedToMakerOrUndefined && connectedToMakerOrUndefined.online && toast.isActive(id)) {
            toast.close(id);
        }
    }, [toast, connectedToMakerOrUndefined]);

    const {
        isOpen: outdatedWarningIsVisible,
        onClose: onCloseOutdatedWarning,
        onOpen: onOpenOutdatedWarning,
    } = useDisclosure();

    useEffect(() => {
        if (githubVersion && daemonVersion && githubVersion > daemonVersion) {
            onOpenOutdatedWarning();
        }
    }, [githubVersion, daemonVersion, onOpenOutdatedWarning]);

    const {
        isOpen: incompatibleWarningIsVisible,
        onClose: onCloseIncompatibleWarning,
        onOpen: onOpenIncompatibleWarning,
    } = useDisclosure({ defaultIsOpen: incompatible });

    useEffect(() => {
        if (incompatible) {
            onOpenIncompatibleWarning();
        }
    }, [incompatible, onOpenIncompatibleWarning]);

    const pathname = location.pathname;
    useEffect(() => {
        let btcusd = Symbol.btcusd;
        if (!pathname.includes("trade") && !pathname.includes("wallet")) {
            navigate(`/trade/${btcusd}/long`);
        }
    }, [navigate, pathname]);

    return (
        <Routes>
            <Route
                path="/"
                element={
                    <MainPageLayout
                        outdatedWarningIsVisible={outdatedWarningIsVisible}
                        incompatibleWarningIsVisible={incompatibleWarningIsVisible}
                        githubVersion={githubVersion}
                        daemonVersion={daemonVersion}
                        onCloseOutdatedWarning={onCloseOutdatedWarning}
                        onCloseIncompatibleWarning={onCloseIncompatibleWarning}
                        connectedToMaker={connectedToMaker}
                        nextFundingEvent={nextFundingEvent}
                        referencePrice={referencePrice}
                        identityOrUndefined={identityOrUndefined}
                        setExtraInfo={setExtraInfo}
                        showExtraInfo={showExtraInfo}
                    />
                }
            >
                <Route
                    path="wallet"
                    element={<Wallet walletInfo={walletInfo} />}
                />
                <Route
                    element={
                        <TradePageLayout
                            cfds={cfds}
                            connectedToMaker={connectedToMaker}
                            showExtraInfo={showExtraInfo}
                        />
                    }
                >
                    <Route
                        path="/trade/:symbol/long"
                        element={
                            <Trade
                                offer={longOffer}
                                connectedToMaker={connectedToMaker}
                                walletBalance={walletInfo ? walletInfo.balance : 0}
                                isLong={true}
                            />
                        }
                    />
                    <Route
                        path="/trade/:symbol/short/"
                        element={
                            <Trade
                                offer={shortOffer}
                                connectedToMaker={connectedToMaker}
                                walletBalance={walletInfo ? walletInfo.balance : 0}
                                isLong={false}
                            />
                        }
                    />
                </Route>
            </Route>
        </Routes>
    );
};

const offer_event_names = (symbol: Symbol) => {
    switch (symbol) {
        case Symbol.ethusd: {
            return {
                long_offer: "ethusd_long_offer",
                short_offer: "ethusd_short_offer",
            };
        }
        case Symbol.btcusd:
        default:
            return {
                long_offer: "btcusd_long_offer",
                short_offer: "btcusd_short_offer",
            };
    }
};

const bitmexWebSocketURL = (symbol: Symbol) => {
    switch (symbol) {
        case Symbol.ethusd:
            return "wss://www.bitmex.com/realtime?subscribe=instrument:.BETH";
        case Symbol.btcusd:
        default:
            return "wss://www.bitmex.com/realtime?subscribe=instrument:.BXBT";
    }
};
