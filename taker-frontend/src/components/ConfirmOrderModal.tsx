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
    Tr,
} from "@chakra-ui/react";
import * as React from "react";
import { useRef } from "react";
import { CfdOrderRequestPayload } from "../types";
import BitcoinAmount from "./BitcoinAmount";
import DollarAmount from "./DollarAmount";
import { InterestRateTooltip } from "./InterestRateTooltip";

interface Props {
    orderId: string;
    position: "long" | "short";
    isOpen: boolean;
    onClose: any;
    onSubmit: (req: CfdOrderRequestPayload) => void;
    isSubmitting: boolean;
    quantity: number;
    price: number;
    margin: number;
    leverage: number;
    liquidationPriceAsNumber: number | undefined;
    feeForFirstSettlementInterval: number;
    interestRateHourly: number;
    interestRateAnnualized: number;
}

export default function ConfirmOrderModal({
    orderId,
    position,
    isOpen,
    onClose,
    onSubmit,
    isSubmitting,
    quantity,
    price,
    margin,
    leverage,
    liquidationPriceAsNumber,
    feeForFirstSettlementInterval,
    interestRateHourly,
    interestRateAnnualized,
}: Props) {
    const confirmRef = useRef<HTMLButtonElement | null>(null);

    let buy_or_sell = "sell";
    if (position === "long") {
        buy_or_sell = "buy";
    }
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
                            Market {buy_or_sell}&nbsp;
                            <b>{quantity}</b> of BTC/USD @
                        </Text>
                        <DollarAmount amount={price} />
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
                                <Td>
                                    <Text as={"b"}>Leverage</Text>
                                </Td>
                                <Td>{leverage}</Td>
                            </Tr>
                            <Tr>
                                <Td>
                                    <Text as={"b"}>Liquidation Price</Text>
                                </Td>
                                <Td>
                                    <DollarAmount amount={liquidationPriceAsNumber || 0} />
                                </Td>
                            </Tr>
                            <Tr>
                                <Td>
                                    <Text as={"b"}>Margin</Text>
                                </Td>
                                <Td>
                                    <BitcoinAmount btc={margin} />
                                </Td>
                            </Tr>
                            <Tr>
                                <Td>
                                    <Text as={"b"}>Funding for first 24h</Text>
                                </Td>
                                <Td>
                                    <BitcoinAmount btc={feeForFirstSettlementInterval} />
                                </Td>
                            </Tr>
                            <InterestRateTooltip
                                interestRateHourly={interestRateHourly}
                                interestRateAnnualized={interestRateAnnualized}
                                disabled={!interestRateHourly}
                            >
                                <Tr>
                                    <Td>
                                        <Text as={"b"}>Interest Rate</Text>
                                    </Td>
                                    <Td>{interestRateHourly}% per hour</Td>
                                </Tr>
                            </InterestRateTooltip>
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
                                onSubmit({
                                    order_id: orderId,
                                    quantity,
                                    position,
                                });

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
