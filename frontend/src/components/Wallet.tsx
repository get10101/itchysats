import { CheckIcon, CopyIcon } from "@chakra-ui/icons";
import { Box, Center, Divider, HStack, IconButton, Skeleton, Text, useClipboard } from "@chakra-ui/react";
import React from "react";
import Timestamp from "./Timestamp";
import { WalletInfo } from "./Types";

interface WalletProps {
    walletInfo: WalletInfo | null;
}

export default function Wallet(
    {
        walletInfo,
    }: WalletProps,
) {
    const { hasCopied, onCopy } = useClipboard(walletInfo ? walletInfo.address : "");

    let balance = <Skeleton height="20px" />;
    let address = <Skeleton height="20px" />;
    let timestamp = <Skeleton height="20px" />;

    if (walletInfo) {
        balance = <Text>{walletInfo.balance} BTC</Text>;
        address = (
            <HStack>
                <Text isTruncated>{walletInfo.address}</Text>
                <IconButton
                    aria-label="Copy to clipboard"
                    icon={hasCopied ? <CheckIcon /> : <CopyIcon />}
                    onClick={onCopy}
                />
            </HStack>
        );
        timestamp = <Timestamp timestamp={walletInfo.last_updated_at} />;
    }

    return (
        <Box shadow={"md"} marginBottom={5} padding={5}>
            <Center><Text fontWeight={"bold"}>Your wallet</Text></Center>
            <HStack>
                <Text align={"left"}>Balance:</Text>
                {balance}
            </HStack>
            <Divider marginTop={2} marginBottom={2} />
            {address}
            <Divider marginTop={2} marginBottom={2} />
            <HStack>
                <Text align={"left"}>Updated:</Text>
                {timestamp}
            </HStack>
        </Box>
    );
}
