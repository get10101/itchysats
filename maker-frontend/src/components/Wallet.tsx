import { CheckIcon, CopyIcon, RepeatIcon } from "@chakra-ui/icons";
import { Box, Center, Divider, HStack, IconButton, Skeleton, Spacer, Text, useClipboard } from "@chakra-ui/react";
import React from "react";
import Timestamp from "./Timestamp";
import { WalletInfo } from "./Types";

interface WalletProps {
    walletInfo: WalletInfo | null;
    syncWallet: () => void;
    isSyncingWallet: boolean;
}

export default function Wallet(
    {
        walletInfo,
        syncWallet,
        isSyncingWallet,
    }: WalletProps,
) {
    const { hasCopied, onCopy } = useClipboard(walletInfo ? walletInfo.address : "");
    const { balance, address, last_updated_at } = walletInfo || {};

    return (
        <Box shadow={"md"} marginBottom={5} padding={5}>
            <Center>
                <Text fontWeight={"bold"}>Your wallet</Text>
            </Center>
            <HStack>
                <Text align={"left"}>Balance:</Text>
                <Skeleton isLoaded={balance != null}>
                    <Text>{balance} BTC</Text>
                </Skeleton>
                <Spacer />
                <IconButton
                    aria-label="Sync Wallet"
                    disabled={balance == null}
                    isLoading={isSyncingWallet}
                    icon={<RepeatIcon />}
                    onClick={syncWallet}
                />
            </HStack>
            <Divider marginTop={2} marginBottom={2} />
            <Skeleton isLoaded={address != null}>
                <HStack>
                    <Text noOfLines={1}>{address}</Text>
                    <IconButton
                        aria-label="Copy to clipboard"
                        icon={hasCopied ? <CheckIcon /> : <CopyIcon />}
                        onClick={onCopy}
                    />
                </HStack>
            </Skeleton>
            <Divider marginTop={2} marginBottom={2} />
            <HStack>
                <Text align={"left"}>Updated:</Text>
                <Skeleton isLoaded={last_updated_at != null}>
                    <Timestamp timestamp={last_updated_at!} />
                </Skeleton>
            </HStack>
        </Box>
    );
}
