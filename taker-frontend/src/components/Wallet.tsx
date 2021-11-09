import { CheckIcon, CopyIcon } from "@chakra-ui/icons";
import { Box, Center, Divider, HStack, IconButton, Skeleton, Text, useClipboard } from "@chakra-ui/react";
import * as React from "react";
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
    const { balance, address, last_updated_at } = walletInfo || {};

    return (
        <Center>
            <Box shadow={"md"} marginBottom={5} padding={5} boxSize={"sm"}>
                <Center><Text fontWeight={"bold"}>Your wallet</Text></Center>
                <HStack>
                    <Text align={"left"}>Balance:</Text>
                    <Skeleton isLoaded={balance != null}>
                        <Text>{balance} BTC</Text>
                    </Skeleton>
                </HStack>
                <Divider marginTop={2} marginBottom={2} />
                <Skeleton isLoaded={address != null}>
                    <HStack>
                        <Text isTruncated>{address}</Text>
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
        </Center>
    );
}

const WalletNavBar = ({
    walletInfo,
}: WalletProps) => {
    const { balance } = walletInfo || {};

    return (
        <HStack>
            <Text align={"left"} as="b">On-chain balance:</Text>
            <Skeleton isLoaded={balance != null}>
                <Text>{balance} BTC</Text>
            </Skeleton>
        </HStack>
    );
};

export { Wallet, WalletNavBar };
