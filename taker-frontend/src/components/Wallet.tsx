import { CheckIcon, CopyIcon } from "@chakra-ui/icons";
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
    useColorModeValue,
    useToast,
    VStack,
} from "@chakra-ui/react";
import * as React from "react";
import { useState } from "react";
import { WalletInfo, WithdrawRequest } from "../types";
import usePostRequest from "../usePostRequest";
import Timestamp from "./Timestamp";

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
    const [runWithdraw, isWithdrawing] = usePostRequest<WithdrawRequest, string>("/api/withdraw", (url) => {
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
    });

    return (
        <Center>
            <Box shadow={"lg"} rounded={"md"} marginBottom={5} padding={5} bg={useColorModeValue("white", "gray.700")}>
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
                            variant={"outline"}
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
                    <form
                        onSubmit={(event) => {
                            event.preventDefault();

                            runWithdraw({
                                amount: withdrawAmount,
                                fee,
                                address: withdrawAddress,
                            });
                        }}
                    >
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
                                    defaultValue={0}
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
                                    defaultValue={0}
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
                        <Button
                            marginTop={5}
                            variant={"solid"}
                            colorScheme={"blue"}
                            type="submit"
                            isLoading={isWithdrawing}
                        >
                            Withdraw
                        </Button>
                    </form>
                </VStack>
            </Box>
        </Center>
    );
}

export { Wallet };
