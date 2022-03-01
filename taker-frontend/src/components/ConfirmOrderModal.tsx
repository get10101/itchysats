import {
    Button,
    HStack,
    Modal,
    ModalBody,
    ModalCloseButton,
    ModalContent,
    ModalFooter,
    ModalHeader,
    ModalOverlay,
    Table,
    TableCaption,
    Tbody,
    Td,
    Text,
    Tooltip,
    Tr,
} from "@chakra-ui/react";
import * as React from "react";
import { CfdOrderRequestPayload } from "../types";
import BitcoinAmount from "./BitcoinAmount";
import DollarAmount from "./DollarAmount";

interface Props {
    orderId: string;
    position: string;
    isOpen: boolean;
    onClose: any;
    onSubmit: (req: CfdOrderRequestPayload) => void;
    isSubmitting: boolean;
    confirmRef: any;
    quantity: number;
    askPriceAsNumber?: number;
    margin: number;
    leverage: number;
    liquidationPriceAsNumber: number | undefined;
    feeForFirstSettlementInterval: number;
    fundingRateHourly: string;
    fundingRateAnnualized: string;
}

export default function ConfirmOrderModal({
    orderId,
    position,
    isOpen,
    onClose,
    onSubmit,
    isSubmitting,
    confirmRef,
    quantity,
    askPriceAsNumber,
    margin,
    leverage,
    liquidationPriceAsNumber,
    feeForFirstSettlementInterval,
    fundingRateHourly,
    fundingRateAnnualized,
}: Props) {
    return (
        <Modal
            isOpen={isOpen}
            onClose={onClose}
            size={"lg"}
            initialFocusRef={confirmRef}
        >
            <ModalOverlay />
            <ModalContent>
                <ModalHeader>
                    <HStack>
                        <Text>
                            Market sell <b>{quantity}</b> of BTC/USD @
                        </Text>
                        <DollarAmount amount={askPriceAsNumber || 0} />
                    </HStack>
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
                                    <BitcoinAmount btc={margin} />
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
                                <Td><DollarAmount amount={liquidationPriceAsNumber || 0} /></Td>
                            </Tr>
                            <Tr>
                                <Td><Text as={"b"}>Margin</Text></Td>
                                <Td><BitcoinAmount btc={margin} /></Td>
                            </Tr>
                            <Tr>
                                <Td><Text as={"b"}>Funding for first 24h</Text></Td>
                                <Td><BitcoinAmount btc={feeForFirstSettlementInterval} /></Td>
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
                        </Tbody>
                    </Table>
                </ModalBody>

                <ModalFooter>
                    <HStack>
                        <Button
                            ref={confirmRef}
                            colorScheme="teal"
                            isLoading={isSubmitting}
                            onClick={() => {
                                let payload: CfdOrderRequestPayload = {
                                    order_id: orderId,
                                    quantity,
                                    position,
                                };
                                onSubmit(payload);

                                onClose();
                            }}
                        >
                            Confirm
                        </Button>
                    </HStack>
                </ModalFooter>
            </ModalContent>
        </Modal>
    );
}
