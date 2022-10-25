import { CheckIcon, CopyIcon, RepeatIcon } from "@chakra-ui/icons";
import {
    Box,
    Button,
    ButtonGroup,
    Center,
    CircularProgress,
    Divider,
    Flex,
    FormControl,
    FormHelperText,
    FormLabel,
    Heading,
    HStack,
    Icon,
    IconButton,
    Input,
    InputGroup,
    InputLeftElement,
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
import axios, { AxiosError } from "axios";
import dayjs from "dayjs";
import FileSaver from "file-saver";
import { QRCodeCanvas } from "qrcode.react";
import * as React from "react";
import { useState } from "react";
import { BsArrowDownRightCircle, BsArrowUpRightCircle } from "react-icons/all";
import { FaSeedling } from "react-icons/fa";
import { Transaction, WalletInfo, WithdrawRequest } from "../types";
import usePostRequest from "../usePostRequest";
import Timestamp from "./Timestamp";

interface WalletProps {
    walletInfo: WalletInfo | null;
    cfds: boolean;
}

interface SeedFile {
    name: string;
    blob: Blob;
}

export default function Wallet(
    {
        walletInfo,
        cfds,
    }: WalletProps,
) {
    const toast = useToast();

    const { hasCopied, onCopy } = useClipboard(walletInfo ? walletInfo.address : "");
    const { balance, address, last_updated_at } = walletInfo || {};

    const [seed, setSeed] = useState<SeedFile | string>("");

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

    let [{ status: importing }, { execute: importSeed }] = useAsync(
        async () => {
            try {
                await axios.put("/api/import", (seed as SeedFile).blob, {
                    withCredentials: true,
                    headers: {
                        "Content-Type": "application/octet-stream",
                    },
                });

                toast({
                    title: "Success: Importing Seed",
                    description: "Successfully imported seed",
                    status: "success",
                    duration: 10000,
                    isClosable: true,
                });
            } catch (e: any) {
                let message = "Unknown error!";
                if (e instanceof AxiosError) {
                    // @ts-ignore
                    message = e.response.data.title;
                }
                toast({
                    title: "Error: Importing Seed",
                    description: message,
                    status: "error",
                    duration: 10000,
                    isClosable: true,
                });
            }
        },
    );
    let isImporting = importing === "loading";

    const exportSeed = () => {
        // downloads the taker seed file directly from the endpoint. No error handling as this is using
        // the native html 5 functionality similar to <a href="/api/export" download></a>
        FileSaver.saveAs("/api/export", "taker_seed", { autoBom: false });

        toast({
            title: "Export successful",
            description: "Keep that safe!",
            status: "success",
            duration: 10000,
            isClosable: true,
        });
    };

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
                </VStack>
                <VStack>
                    <HStack marginTop={"5"} marginBottom={"5"}>
                        <Button
                            variant={"solid"}
                            colorScheme={"blue"}
                            isLoading={isWithdrawing}
                            onClick={() => runWithdraw([{ amount: withdrawAmount, fee, address: withdrawAddress }])}
                        >
                            Withdraw
                        </Button>
                    </HStack>
                </VStack>

                {walletInfo?.managed_wallet && (
                    <Box>
                        <Divider marginTop={3} marginBottom={4} />
                        <Center>
                            <Heading size={"sm"}>Import / Export Seed</Heading>
                        </Center>

                        <VStack padding={2}>
                            <HStack>
                                <FormControl id="seed">
                                    <FormLabel>Seed</FormLabel>
                                    <input
                                        type="file"
                                        id={"importSeed"}
                                        style={{ display: "none" }}
                                        onChange={(event) => {
                                            if (
                                                !event.target || !event.target.files || event.target.files.length === 0
                                            ) {
                                                setSeed("");
                                                return;
                                            }
                                            const file = event.target.files[0];

                                            if (file.size !== 256) {
                                                toast({
                                                    title: "Error: Incorrect seed file",
                                                    description:
                                                        "The selected seed file needs to be exactly 256 bytes long.",
                                                    status: "error",
                                                    duration: 10000,
                                                    isClosable: true,
                                                });
                                                return;
                                            }

                                            const fileReader = new FileReader();

                                            fileReader.readAsArrayBuffer(file);
                                            fileReader.onload = () => {
                                                setSeed({
                                                    name: file.name,
                                                    blob: new Blob([fileReader.result as ArrayBuffer], {
                                                        // This will set the mimetype of the file
                                                        type: "application/octet-stream",
                                                    }),
                                                });
                                            };
                                        }}
                                    />
                                    <InputGroup>
                                        <InputLeftElement
                                            pointerEvents="none"
                                            children={<Icon as={FaSeedling} />}
                                        />
                                        <Input
                                            placeholder="Click here to import your seed ..."
                                            onClick={() => {
                                                const element = document.getElementById("importSeed");
                                                if (element) {
                                                    element.click();
                                                }
                                            }}
                                            onChange={(change) => {}}
                                            value={seed ? (seed as SeedFile).name : ""}
                                            isDisabled={cfds}
                                        />
                                    </InputGroup>
                                    <FormHelperText>
                                        You can not import your seed after CFDs have been created.
                                    </FormHelperText>
                                </FormControl>
                                <Box pt={2}>
                                    <ButtonGroup>
                                        <Button
                                            variant={"solid"}
                                            colorScheme={"blue"}
                                            onClick={async () => {
                                                await importSeed();
                                                setSeed("");
                                                const element = document.getElementById("importSeed");
                                                if (element) {
                                                    // clear selected value from upload field.
                                                    // @ts-ignore
                                                    element.value = "";
                                                }
                                            }}
                                            isLoading={isImporting}
                                            disabled={seed === "" || isImporting || cfds}
                                            size={"md"}
                                        >
                                            Import
                                        </Button>

                                        <Button
                                            variant={"solid"}
                                            colorScheme={"blue"}
                                            onClick={exportSeed}
                                            size={"md"}
                                        >
                                            Export
                                        </Button>
                                    </ButtonGroup>
                                </Box>
                            </HStack>
                        </VStack>
                    </Box>
                )}

                <Divider marginTop={7} marginBottom={2} />
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
