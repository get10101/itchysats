import { CheckIcon, CopyIcon, RepeatIcon } from "@chakra-ui/icons";
import {
    Box,
    Button,
    Center,
    CircularProgress,
    Divider,
    Flex,
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
    Spacer,
    Text,
    useClipboard,
    useColorModeValue,
    useToast,
    VStack,
} from "@chakra-ui/react";
import { useAsync } from "@react-hookz/web";
import axios from "axios";
import dayjs from "dayjs";
import { QRCodeCanvas } from "qrcode.react";
import * as React from "react";
import { useState } from "react";
import { BsArrowDownRightCircle, BsArrowUpRightCircle } from "react-icons/all";
import { Transaction, WalletInfo, WithdrawRequest } from "../types";
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
            description: (
                <Link href={url} isExternal>
                    {url}
                </Link>
            ),
            status: "info",
            duration: 10000,
            isClosable: true,
        });
    });

    let [{ status: walletSyncing }, { execute: syncWallet }] = useAsync(
        async () => {
            try {
                let res = await axios.put("/api/sync", {}, { withCredentials: true });

                if (!res.status.toString().startsWith("2")) {
                    console.log("Status: " + res.status + ", " + res.statusText);
                    const resp = res.data;
                    toast({
                        title: "Error: Syncing Wallet",
                        description: resp.description,
                        status: "error",
                        duration: 10000,
                        isClosable: true,
                    });
                }
            } catch (e: any) {
                toast({
                    title: "Error: Syncing Wallet",
                    description: e.detail,
                    status: "error",
                    duration: 10000,
                    isClosable: true,
                });
            }
        },
    );
    let isSyncingWallet = walletSyncing === "loading";

    return (
        <Center>
            <Box shadow={"lg"} rounded={"md"} marginBottom={5} padding={5} bg={useColorModeValue("white", "gray.700")}>
                <Center>
                    <Heading size="sm">Wallet Details</Heading>
                </Center>
                <HStack spacing={8} direction="row" align="stretch">
                    <Box>
                        <HStack>
                            <Text align={"left"}>Balance:</Text>
                            <Skeleton isLoaded={balance != null}>
                                <Text>{balance} BTC</Text>
                            </Skeleton>
                            <Spacer />
                            <IconButton
                                aria-label="Sync Wallet"
                                variant={"outline"}
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
                    </Box>
                    <Spacer />
                    <Box>
                        <QRCodeCanvas value={`bitcoin:${address!}`} />
                    </Box>
                </HStack>

                <Divider marginTop={2} marginBottom={2} />

                <VStack padding={2}>
                    <form
                        onSubmit={(event) => {
                            event.preventDefault();
                            runWithdraw([{
                                amount: withdrawAmount,
                                fee,
                                address: withdrawAddress,
                            }]);
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

                <Divider marginTop={2} marginBottom={2} />
                <Box>
                    <Center>
                        <Heading size={"sm"}>History</Heading>
                    </Center>
                    <Transactions transactions={walletInfo?.transactions} />
                </Box>
            </Box>
        </Center>
    );
}

export { Wallet };

interface TransactionsProps {
    transactions?: Transaction[];
}

function Transactions({ transactions }: TransactionsProps) {
    if (!transactions) {
        return <Text>You have no transactions so far</Text>;
    }
    let transaction = transactions.sort((a, b) => {
        if (!a.confirmation_time) {
            return -1;
        } else if (!b.confirmation_time) {
            return +1;
        } else {
            return b.confirmation_time.timestamp - a.confirmation_time.timestamp;
        }
    }).map((tx) => {
        let symbol = <BsArrowDownRightCircle color={"green"} size={"24px"} />;
        let outgoing = false;
        if (tx.sent > 0) {
            symbol = <BsArrowUpRightCircle color={"red"} size={"24px"} />;
            outgoing = true;
        }
        let timeSince;
        if (tx.confirmation_time) {
            timeSince = dayjs.unix(tx.confirmation_time.timestamp).fromNow();
        } else {
            timeSince = "Pending...";
            symbol = <CircularProgress isIndeterminate color="gray" size={"24px"} />;
        }

        let maybeLink = <></>;
        if (tx.link) {
            maybeLink = (
                <Link href={`${tx.link}`} isExternal>
                    {symbol}
                </Link>
            );
        }
        return (
            <Box key={tx.txid}>
                <Flex w={"100%"} padding={"0.5em"} gap={"2"}>
                    {maybeLink}
                    <Text>{timeSince}</Text>
                    <Spacer />
                    <Text>{outgoing ? "-" + tx.sent : "+" + tx.received} BTC</Text>
                </Flex>
                <Divider marginTop={2} marginBottom={2} />
            </Box>
        );
    });
    return <>{transaction}</>;
}
