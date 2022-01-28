import { ExternalLinkIcon } from "@chakra-ui/icons";
import { BoxProps } from "@chakra-ui/layout";
import {
    Box,
    Button,
    ButtonGroup,
    Center,
    Circle,
    FormControl,
    FormHelperText,
    FormLabel,
    Grid,
    GridItem,
    HStack,
    IconButton,
    InputGroup,
    InputLeftAddon,
    Modal,
    ModalBody,
    ModalCloseButton,
    ModalContent,
    ModalFooter,
    ModalHeader,
    ModalOverlay,
    NumberDecrementStepper,
    NumberIncrementStepper,
    NumberInput,
    NumberInputField,
    NumberInputStepper,
    Skeleton,
    Slider,
    SliderFilledTrack,
    SliderThumb,
    SliderTrack,
    Table,
    TableCaption,
    Tbody,
    Td,
    Text,
    Tooltip,
    Tr,
    useColorModeValue,
    useDisclosure,
    VStack,
} from "@chakra-ui/react";
import { motion } from "framer-motion";
import * as React from "react";
import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { CfdOrderRequestPayload, ConnectionStatus } from "../types";
import usePostRequest from "../usePostRequest";
import AlertBox from "./AlertBox";
import BitcoinAmount from "./BitcoinAmount";

const MotionBox = motion<BoxProps>(Box);

interface TradeProps {
    connectedToMaker: ConnectionStatus;
    orderId?: string;
    minQuantity: number;
    maxQuantity: number;
    referencePrice?: number;
    askPrice?: number;
    parcelSize: number;
    marginPerParcel: number;
    leverage?: number;
    liquidationPrice?: number;
    walletBalance: number;
    openingFeePerParcel: number;
    fundingRateAnnualized: string;
    fundingRateHourly: string;
}

export default function Trade({
    connectedToMaker,
    minQuantity,
    maxQuantity,
    askPrice: askPriceAsNumber,
    parcelSize,
    marginPerParcel,
    leverage,
    liquidationPrice: liquidationPriceAsNumber,
    orderId,
    walletBalance,
    openingFeePerParcel,
    fundingRateAnnualized,
    fundingRateHourly,
}: TradeProps) {
    let [quantity, setQuantity] = useState(0);
    let [userHasEdited, setUserHasEdited] = useState(false);

    // We update the quantity because the offer can change any time.
    useEffect(() => {
        if (!userHasEdited) {
            setQuantity(minQuantity);
        }
    }, [userHasEdited, minQuantity, setQuantity]);

    let [onLongSubmit, isLongSubmitting] = usePostRequest<CfdOrderRequestPayload>("/api/cfd/order");

    let outerCircleBg = useColorModeValue("gray.100", "gray.700");
    let innerCircleBg = useColorModeValue("gray.200", "gray.600");

    // Use `Number` wrapper here to ensure the "," separator is printed.
    const askPrice = `$${Number(askPriceAsNumber)?.toLocaleString() || "0.0"}`;
    const liquidationPrice = `$${Number(liquidationPriceAsNumber)?.toLocaleString() || "0.0"}`;

    const { isOpen, onOpen, onClose } = useDisclosure();

    const margin = (quantity / parcelSize) * marginPerParcel;

    const openingFee = (quantity / parcelSize) * openingFeePerParcel;

    let btcToOpenPosition = margin + openingFee;

    const balanceTooLow = walletBalance < btcToOpenPosition;
    const quantityTooHigh = maxQuantity < quantity;
    const quantityTooLow = minQuantity > quantity;
    const quantityGreaterZero = quantity > 0;
    const quantityIsEvenlyDivisibleByIncrement = isEvenlyDivisible(quantity, parcelSize);

    const canSubmit = orderId && !isLongSubmitting && !balanceTooLow
        && !quantityTooHigh && !quantityTooLow && quantityGreaterZero && quantityIsEvenlyDivisibleByIncrement;

    let alertBox;

    if (!connectedToMaker.online) {
        alertBox = <AlertBox
            title={"No maker!"}
            description={"You are not connected to any maker. Functionality may be limited"}
        />;
    } else {
        if (balanceTooLow) {
            alertBox = <AlertBox
                title={"Your balance is too low!"}
                description={"Please deposit more into your wallet."}
            />;
        }
        if (!quantityIsEvenlyDivisibleByIncrement) {
            alertBox = <AlertBox
                title={`Quantity is not in increments of ${parcelSize}!`}
                description={`Increment is ${parcelSize}`}
            />;
        }
        if (quantityTooHigh) {
            alertBox = <AlertBox
                title={"Quantity too high!"}
                description={`Max available liquidity is ${maxQuantity}`}
            />;
        }
        if (quantityTooLow || !quantityGreaterZero) {
            alertBox = <AlertBox title={"Quantity too low!"} description={`Min quantity is ${minQuantity}`} />;
        }
        if (!orderId) {
            alertBox = <AlertBox
                title={"No liquidity!"}
                description={"The maker you are connected has not create any offers"}
            />;
        }
    }

    const confirmRef = useRef<HTMLButtonElement | null>(null);

    return (
        <VStack>
            <Center>
                <Grid
                    templateRows="repeat(1, 1fr)"
                    templateColumns="repeat(1, 1fr)"
                    gap={4}
                    maxWidth={"500px"}
                >
                    <GridItem colSpan={1}>
                        <Center>
                            <MotionBox
                                variants={{
                                    pulse: {
                                        scale: [1, 1.05, 1],
                                    },
                                }}
                                // @ts-ignore: lint is complaining but should be fine :)
                                transition={{
                                    // type: "spring",
                                    ease: "linear",
                                    duration: 2,
                                    repeat: Infinity,
                                }}
                                animate={"pulse"}
                            >
                                <Circle size="256px" bg={outerCircleBg}>
                                    <Circle size="180px" bg={innerCircleBg}>
                                        <MotionBox>
                                            <Skeleton isLoaded={!!askPriceAsNumber && askPriceAsNumber > 0}>
                                                <Text fontSize={"4xl"} as="b">{askPrice}</Text>
                                            </Skeleton>
                                        </MotionBox>
                                    </Circle>
                                </Circle>
                            </MotionBox>
                        </Center>
                    </GridItem>
                    <GridItem colSpan={1} paddingLeft={5} paddingRight={5}>
                        <Quantity
                            min={minQuantity}
                            max={maxQuantity}
                            quantity={quantity}
                            onChange={(_valueAsString: string, valueAsNumber: number) => {
                                setQuantity(Number.isNaN(valueAsNumber) ? 0 : valueAsNumber);
                                setUserHasEdited(true);
                            }}
                            parcelSize={parcelSize}
                        />
                    </GridItem>
                    <GridItem colSpan={1} paddingLeft={5} paddingRight={5}>
                        <Leverage leverage={leverage} />
                    </GridItem>
                    <GridItem colSpan={1}>
                        <OpeningDetails
                            margin={margin}
                            walletBalance={walletBalance}
                        />
                    </GridItem>
                    <GridItem colSpan={1}>
                        <Center>
                            <ButtonGroup
                                variant="solid"
                                padding="3"
                                spacing="6"
                            >
                                <Button colorScheme="red" size="lg" disabled h={16} w={"40"} fontSize={"xl"}>
                                    Short
                                </Button>
                                <Button
                                    disabled={!canSubmit}
                                    colorScheme="green"
                                    size="lg"
                                    onClick={onOpen}
                                    h={16}
                                    w={"40"}
                                    fontSize={"xl"}
                                >
                                    Long
                                </Button>

                                <Modal isOpen={isOpen} onClose={onClose} size={"lg"} initialFocusRef={confirmRef}>
                                    <ModalOverlay />
                                    <ModalContent>
                                        <ModalHeader>
                                            Market buy <b>{quantity}</b> of BTC/USD @ <b>{askPrice}</b>
                                        </ModalHeader>
                                        <ModalCloseButton />
                                        <ModalBody>
                                            <Table variant="striped" colorScheme="gray" size="sm">
                                                <TableCaption>
                                                    <HStack>
                                                        <Text>
                                                            By submitting
                                                        </Text>
                                                        <Text as={"b"}>
                                                            <BitcoinAmount btc={btcToOpenPosition} />
                                                        </Text>
                                                        <Text>
                                                            will be locked on-chain in a contract.
                                                        </Text>
                                                    </HStack>
                                                </TableCaption>
                                                <Tbody>
                                                    <Tr>
                                                        <Td><Text as={"b"}>Leverage</Text></Td>
                                                        <Td>{leverage}</Td>
                                                    </Tr>
                                                    <Tr>
                                                        <Td><Text as={"b"}>Liquidation Price</Text></Td>
                                                        <Td>{liquidationPrice}</Td>
                                                    </Tr>
                                                    <Tooltip
                                                        label={`The CFD is rolled over perpetually every hour at ${fundingRateHourly}%, annualized that is ${fundingRateAnnualized}%. The funding rate can fluctuate depending on the market movements.`}
                                                        hasArrow
                                                        placement={"right"}
                                                    >
                                                        <Tr>
                                                            <Td><Text as={"b"}>Perpetual Costs</Text></Td>
                                                            <Td>Hourly @ {fundingRateHourly}%</Td>
                                                        </Tr>
                                                    </Tooltip>
                                                    <Tr>
                                                        <Td><Text as={"b"}>Margin</Text></Td>
                                                        <Td><BitcoinAmount btc={margin} /></Td>
                                                    </Tr>
                                                    <Tr>
                                                        <Td><Text as={"b"}>Opening Fee</Text></Td>
                                                        <Td><BitcoinAmount btc={openingFee} /></Td>
                                                    </Tr>
                                                </Tbody>
                                            </Table>
                                        </ModalBody>

                                        <ModalFooter>
                                            <HStack>
                                                <Button
                                                    ref={confirmRef}
                                                    colorScheme="teal"
                                                    isLoading={isLongSubmitting}
                                                    onClick={() => {
                                                        let payload: CfdOrderRequestPayload = {
                                                            order_id: orderId!,
                                                            quantity,
                                                        };
                                                        onLongSubmit(payload);

                                                        setQuantity(minQuantity);
                                                        setUserHasEdited(false);

                                                        onClose();
                                                    }}
                                                >
                                                    Confirm
                                                </Button>
                                            </HStack>
                                        </ModalFooter>
                                    </ModalContent>
                                </Modal>
                            </ButtonGroup>
                        </Center>
                    </GridItem>
                </Grid>
            </Center>
            {alertBox}
        </VStack>
    );
}

interface QuantityProps {
    min: number;
    max: number;
    quantity: number;
    parcelSize: number;
    onChange: (valueAsString: string, valueAsNumber: number) => void;
}

function Quantity({ min, max, onChange, quantity, parcelSize }: QuantityProps) {
    return (
        <FormControl id="quantity">
            <FormLabel>Quantity</FormLabel>
            <InputGroup>
                <InputLeftAddon>$</InputLeftAddon>
                <NumberInput
                    min={min}
                    max={max}
                    default={min}
                    step={parcelSize}
                    onChange={onChange}
                    value={quantity}
                    w={"100%"}
                >
                    <NumberInputField />
                    <NumberInputStepper>
                        <NumberIncrementStepper />
                        <NumberDecrementStepper />
                    </NumberInputStepper>
                </NumberInput>
            </InputGroup>
            <FormHelperText>How much do you want to buy or sell?</FormHelperText>
        </FormControl>
    );
}

interface LeverageProps {
    leverage?: number;
}

function Leverage({ leverage }: LeverageProps) {
    return (
        <FormControl id="leverage">
            <FormLabel>Leverage</FormLabel>
            <Slider disabled value={leverage} min={1} max={5} step={1}>
                <SliderTrack>
                    <Box position="relative" right={10} />
                    <SliderFilledTrack />
                </SliderTrack>
                <SliderThumb boxSize={6}>
                    <Text color="black">{leverage}</Text>
                </SliderThumb>
            </Slider>
            <FormHelperText>
                How much do you want to leverage your position?
            </FormHelperText>
        </FormControl>
    );
}

interface OpeningDetailsProps {
    margin: number;
    walletBalance: number;
}

function OpeningDetails({ margin, walletBalance }: OpeningDetailsProps) {
    const navigate = useNavigate();
    return (
        <Table variant="simple">
            <Tbody>
                <Tr>
                    <Td>Required Margin</Td>
                    <Td isNumeric><BitcoinAmount btc={margin} /></Td>
                </Tr>
                <Tr>
                    <Td>
                        <HStack>
                            <Text>Available Balance</Text>
                            <IconButton
                                variant={"unstyled"}
                                aria-label="Go to wallet"
                                icon={<ExternalLinkIcon />}
                                onClick={() => navigate("/wallet")}
                            />
                        </HStack>
                    </Td>
                    <Td isNumeric>
                        <BitcoinAmount btc={walletBalance} />
                    </Td>
                </Tr>
            </Tbody>
        </Table>
    );
}

export function isEvenlyDivisible(numerator: number, divisor: number): boolean {
    return (numerator % divisor === 0.0);
}
