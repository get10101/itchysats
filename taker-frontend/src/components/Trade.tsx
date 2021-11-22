import { BoxProps } from "@chakra-ui/layout";
import {
    Alert,
    AlertDescription,
    AlertIcon,
    AlertTitle,
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
import { useAsync } from "react-async";
import { CfdOrderRequestPayload } from "./Types";

const MotionBox = motion<BoxProps>(Box);

interface TradeProps {
    orderId?: string;
    minQuantity: number;
    maxQuantity: number;
    referencePrice?: number;
    askPrice?: number;
    margin?: string;
    leverage?: number;
    quantity: string;
    liquidationPrice?: number;
    isSubmitting: boolean;
    onQuantityChange: any;
    walletBalance?: number;
    onLongSubmit: (payload: CfdOrderRequestPayload) => void;
}

interface AlertBoxProps {
    title: string;
    description: string;
}

function AlertBox({ title, description }: AlertBoxProps) {
    return (<Alert status="error">
        <AlertIcon />
        <AlertTitle mr={2}>{title}</AlertTitle>
        <AlertDescription>{description}</AlertDescription>
    </Alert>);
}

const Trade = (
    {
        minQuantity,
        maxQuantity,
        referencePrice: referencePriceAsNumber,
        askPrice: askPriceAsNumber,
        quantity,
        onQuantityChange,
        margin: marginAsNumber,
        leverage,
        liquidationPrice: liquidationPriceAsNumber,
        onLongSubmit,
        orderId,
        walletBalance,
    }: TradeProps,
) => {
    let outerCircleBg = useColorModeValue("gray.100", "gray.700");
    let innerCircleBg = useColorModeValue("gray.200", "gray.600");

    const referencePrice = `$${referencePriceAsNumber?.toLocaleString() || "0.0"}`;
    const askPrice = `$${askPriceAsNumber?.toLocaleString() || "0.0"}`;
    const liquidationPrice = `$${liquidationPriceAsNumber?.toLocaleString() || "0.0"}`;
    const margin = `₿${marginAsNumber?.toLocaleString() || "0.0"}`;

    const { isOpen, onOpen, onClose } = useDisclosure();

    let { run: goLong, isLoading: isSubmitting } = useAsync({
        deferFn: async () => {
            const quantityAsNumber = quantity.replace("$", "");

            let payload: CfdOrderRequestPayload = {
                order_id: orderId!,
                quantity: Number.parseFloat(quantityAsNumber),
            };
            await onLongSubmit(payload);
            onClose();
        },
    });

    const parse = (val: any) => Number.parseInt(val.replace(/^\$/, ""));

    const balanceTooLow = walletBalance && walletBalance < parse(margin);
    const quantityTooHigh = maxQuantity < parse(quantity);
    const quantityTooLow = minQuantity > parse(quantity);
    const quantityGreaterZero = parse(quantity) > 0;

    const canSubmit = orderId && !isSubmitting && !balanceTooLow
        && !quantityTooHigh && !quantityTooLow && quantityGreaterZero;

    let alertBox;

    if (balanceTooLow) {
        alertBox = <AlertBox title={"Your balance is too low!"} description={"Pleas deposit more into you wallet."} />;
    }
    if (quantityTooHigh) {
        alertBox = <AlertBox title={"Quantity too high!"} description={`Max available liquidity is ${maxQuantity}`} />;
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

    return (
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
                    <Quantity min={minQuantity} max={maxQuantity} quantity={quantity} onChange={onQuantityChange} />
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
                            <Button colorScheme="red" size="lg" disabled h={16}>
                                <VStack>
                                    <Text as="b">Short</Text>
                                    <Text fontSize={"sm"}>{quantity.replace("$", "")}@{askPrice}</Text>
                                </VStack>
                            </Button>
                            <Button disabled={!canSubmit} colorScheme="green" size="lg" onClick={onOpen} h={16}>
                                <VStack>
                                    <Text as="b">Long</Text>
                                    <Text fontSize={"sm"}>{quantity.replace("$", "")}@{askPrice}</Text>
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
                                                By submitting, {margin} will be locked on-chain in a contract.
                                            </TableCaption>
                                            <Tbody>
                                                <Tr>
                                                    <Td><Text as={"b"}>Margin</Text></Td>
                                                    <Td>{margin}</Td>
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
                                            <Button colorScheme="teal" isLoading={isSubmitting} onClick={goLong}>
                                                Confirm
                                            </Button>
                                        </HStack>
                                    </ModalFooter>
                                </ModalContent>
                            </Modal>
                        </ButtonGroup>
                    </Center>
                    {alertBox}
                </GridItem>
            </Grid>
        </Center>
    );
};
export default Trade;

interface QuantityProps {
    min: number;
    max: number;
    quantity: string;
    onChange: any;
}

function Quantity({ min, max, onChange, quantity }: QuantityProps) {
    return (
        <FormControl id="quantity">
            <FormLabel>Quantity</FormLabel>
            <NumberInput
                min={min}
                max={max}
                default={min}
                onChange={onChange}
                value={quantity}
            >
                <NumberInputField />
                <NumberInputStepper>
                    <NumberIncrementStepper />
                    <NumberDecrementStepper />
                </NumberInputStepper>
            </NumberInput>
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
    margin?: string;
}

function Margin({ margin }: MarginProps) {
    return (
        <VStack>
            <HStack>
                <Text as={"b"}>Required margin:</Text>
                <Text>{margin}</Text>
            </HStack>
            <Text fontSize={"sm"} color={"darkgrey"}>The collateral you will need to provide</Text>
        </VStack>
    );
}
