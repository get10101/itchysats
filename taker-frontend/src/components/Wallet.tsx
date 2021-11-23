import { CheckIcon, CopyIcon, ExternalLinkIcon } from "@chakra-ui/icons";
import {
    Box,
    Button,
    Center,
    Divider,
    FormControl,
    FormHelperText,
    FormLabel,
    Heading,
    HStack,
    IconButton,
    Input,
    Link,
    NumberDecrementStepper,
    NumberIncrementStepper,
    NumberInput,
    NumberInputField,
    NumberInputStepper,
    Skeleton,
    Text,
    useClipboard,
    useToast,
    VStack,
} from "@chakra-ui/react";
import * as React from "react";
import { FormEvent, useState } from "react";
import { useAsync } from "react-async";
import { useNavigate } from "react-router-dom";
import { postWithdraw } from "../App";
import createErrorToast from "./ErrorToast";
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
    const toast = useToast();
    const { hasCopied, onCopy } = useClipboard(walletInfo ? walletInfo.address : "");
    const { balance, address, last_updated_at } = walletInfo || {};

    const [withdrawAmount, setWithdrawAmount] = useState(0);
    const [fee, setFee] = useState(1);
    const [withdrawAddress, setWithdrawAddress] = useState("");

    let { run: runWithdraw, isLoading: isWithdrawing } = useAsync({
        deferFn: async ([event]: FormEvent<HTMLFormElement>[]) => {
            event.preventDefault();
            try {
                const url = await postWithdraw({
                    amount: withdrawAmount,
                    fee,
                    address: withdrawAddress,
                });
                window.open(url, "_blank");
                toast({
                    title: "Withdraw successful",
                    description: <Link href={url} isExternal>
                        {url}
                    </Link>,
                    status: "info",
                    duration: 10000,
                    isClosable: true,
                });
            } catch (e) {
                createErrorToast(toast, e);
            }
        },
    });

    return (
        <Center>
            <Box shadow={"md"} marginBottom={5} padding={5}>
                <Center>
                    <Heading size="sm">Wallet Details</Heading>
                </Center>
                <HStack padding={2}>
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

                <Divider marginTop={2} marginBottom={2} />

                <VStack padding={2}>
                    <form onSubmit={runWithdraw}>
                        <Heading as="h3" size="sm">Withdraw</Heading>
                        <FormControl id="address">
                            <FormLabel>Address</FormLabel>
                            <Input
                                onChange={(event) => setWithdrawAddress(event.target.value)}
                                value={withdrawAddress}
                                placeholder="Target address"
                            >
                            </Input>
                        </FormControl>
                        <HStack>
                            <FormControl id="amount">
                                <FormLabel>Amount</FormLabel>
                                <NumberInput
                                    min={0}
                                    max={balance}
                                    default={0}
                                    onChange={(_, amount) => setWithdrawAmount(amount)}
                                    value={withdrawAmount}
                                    precision={8}
                                    step={0.001}
                                    placeholder="How much do you want to withdraw? (0 to withdraw all)"
                                >
                                    <NumberInputField />
                                    <NumberInputStepper>
                                        <NumberIncrementStepper />
                                        <NumberDecrementStepper />
                                    </NumberInputStepper>
                                </NumberInput>
                                <FormHelperText>How much do you want to withdraw? (0 to withdraw all)</FormHelperText>
                            </FormControl>
                            <FormControl id="fee" w={"30%"}>
                                <FormLabel>Fee</FormLabel>
                                <NumberInput
                                    min={1}
                                    max={100}
                                    default={0}
                                    onChange={(_, amount) => setFee(amount)}
                                    value={fee}
                                    step={1}
                                    placeholder="In sats/vbyte"
                                >
                                    <NumberInputField />
                                    <NumberInputStepper>
                                        <NumberIncrementStepper />
                                        <NumberDecrementStepper />
                                    </NumberInputStepper>
                                </NumberInput>
                                <FormHelperText>In sats/vbyte</FormHelperText>
                            </FormControl>
                        </HStack>
                        <Button type="submit" isLoading={isWithdrawing}>Withdraw</Button>
                    </form>
                </VStack>
            </Box>
        </Center>
    );
}

const WalletInfoBar = ({
    walletInfo,
}: WalletProps) => {
    const { balance } = walletInfo || {};
    const navigate = useNavigate();

    return (
        <HStack>
            <Text align={"left"} as="b">Wallet Balance:</Text>
            <Skeleton isLoaded={balance != null}>
                <HStack>
                    <Text>{balance} BTC</Text>
                    <IconButton
                        bg={"transparent"}
                        aria-label="Go to wallet"
                        icon={<ExternalLinkIcon />}
                        onClick={() => navigate("/wallet")}
                    />
                </HStack>
            </Skeleton>
        </HStack>
    );
};

export { Wallet, WalletInfoBar };
