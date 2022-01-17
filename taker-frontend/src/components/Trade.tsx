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
import { useEffect, useState } from "react";
import { CfdOrderRequestPayload, ConnectionStatus } from "../types";
import usePostRequest from "../usePostRequest";
import AlertBox from "./AlertBox";

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
}

export default function Trade({
    connectedToMaker,
    minQuantity,
    maxQuantity,
    referencePrice: referencePriceAsNumber,
    askPrice: askPriceAsNumber,
    parcelSize,
    marginPerParcel,
    leverage,
    liquidationPrice: liquidationPriceAsNumber,
    orderId,
    walletBalance,
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

    const referencePrice = `$${referencePriceAsNumber?.toLocaleString() || "0.0"}`;
    const askPrice = `$${askPriceAsNumber?.toLocaleString() || "0.0"}`;
    const liquidationPrice = `$${liquidationPriceAsNumber?.toLocaleString() || "0.0"}`;

    const { isOpen, onOpen, onClose } = useDisclosure();

    const margin = (quantity / parcelSize) * marginPerParcel;

    const balanceTooLow = walletBalance < margin;
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

    return (
        <VStack>
            <Center>
                <Grid
                    templateRows="repeat(1, 1fr)"
                    templateColumns="repeat(1, 1fr)"
                    gap={4}
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
                                            <Skeleton isLoaded={!!referencePriceAsNumber && referencePriceAsNumber > 0}>
                                                <Text fontSize={"4xl"} as="b">{referencePrice}</Text>
                                            </Skeleton>
                                        </MotionBox>
                                    </Circle>
                                </Circle>
                            </MotionBox>
                        </Center>
                    </GridItem>
                    <GridItem colSpan={1}>
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
                    <GridItem colSpan={1}>
                        <Leverage leverage={leverage} />
                    </GridItem>
                    <GridItem colSpan={1}>
                        <Margin margin={margin} />
                    </GridItem>
                    <GridItem colSpan={1}>
                        <Center>
                            <ButtonGroup
                                variant="solid"
                                padding="3"
                                spacing="6"
                            >
                                <Button colorScheme="red" size="lg" disabled h={16} w={"40"}>
                                    <VStack>
                                        <Text as="b">Short</Text>
                                        <Text fontSize={"sm"}>{quantity}@{askPrice}</Text>
                                    </VStack>
                                </Button>
                                <Button
                                    disabled={!canSubmit}
                                    colorScheme="green"
                                    size="lg"
                                    onClick={onOpen}
                                    h={16}
                                    w={"40"}
                                >
                                    <VStack>
                                        <Text as="b">Long</Text>
                                        <Text fontSize={"sm"}>{quantity}@{askPrice}</Text>
                                    </VStack>
                                </Button>

                                <Modal isOpen={isOpen} onClose={onClose}>
                                    <ModalOverlay />
                                    <ModalContent>
                                        <ModalHeader>
                                            Market buy <b>{quantity}</b> of BTC/USD @ <b>{askPrice}</b>
                                        </ModalHeader>
                                        <ModalCloseButton />
                                        <ModalBody>
                                            <Table variant="striped" colorScheme="gray" size="sm">
                                                <TableCaption>
                                                    By submitting, ₿{margin} will be locked on-chain in a contract.
                                                </TableCaption>
                                                <Tbody>
                                                    <Tr>
                                                        <Td><Text as={"b"}>Margin</Text></Td>
                                                        <Td>₿{margin}</Td>
                                                    </Tr>
                                                    <Tr>
                                                        <Td><Text as={"b"}>Leverage</Text></Td>
                                                        <Td>{leverage}</Td>
                                                    </Tr>
                                                    <Tr>
                                                        <Td><Text as={"b"}>Liquidation Price</Text></Td>
                                                        <Td>{liquidationPrice}</Td>
                                                    </Tr>
                                                </Tbody>
                                            </Table>
                                        </ModalBody>

                                        <ModalFooter>
                                            <HStack>
                                                <Button
                                                    colorScheme="teal"
                                                    isLoading={isLongSubmitting}
                                                    onClick={() => {
                                                        let payload: CfdOrderRequestPayload = {
                                                            order_id: orderId!,
                                                            quantity,
                                                        };
                                                        onLongSubmit(payload);
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
            <Tooltip label="Configurable leverage is in the making." shouldWrapChildren hasArrow>
                <Slider disabled value={leverage} min={1} max={5} step={1}>
                    <SliderTrack>
                        <Box position="relative" right={10} />
                        <SliderFilledTrack />
                    </SliderTrack>
                    <SliderThumb boxSize={6}>
                        <Text color="black">{leverage}</Text>
                    </SliderThumb>
                </Slider>
            </Tooltip>
            <FormHelperText>
                How much do you want to leverage your position?
            </FormHelperText>
        </FormControl>
    );
}

interface MarginProps {
    margin: number;
}

function Margin({ margin }: MarginProps) {
    return (
        <VStack>
            <HStack>
                <Text as={"b"}>Required margin:</Text>
                <Text>₿{margin}</Text>
            </HStack>
            <Text fontSize={"sm"} color={"darkgrey"}>The collateral you will need to provide</Text>
        </VStack>
    );
}

export function isEvenlyDivisible(numerator: number, divisor: number): boolean {
    return (numerator % divisor === 0.0);
}
