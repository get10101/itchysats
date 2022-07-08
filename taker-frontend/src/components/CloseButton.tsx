import {
    Box,
    Button,
    Popover,
    PopoverArrow,
    PopoverBody,
    PopoverCloseButton,
    PopoverContent,
    PopoverFooter,
    PopoverHeader,
    PopoverTrigger,
    Text,
} from "@chakra-ui/react";
import * as React from "react";
import { Cfd, StateGroupKey, StateKey } from "../types";
import Timestamp from "./Timestamp";

interface Props {
    cfd: Cfd;
    request: ([req]: [any]) => void;
    status: boolean;
    isForceCloseButton: boolean;
    buttonTitle: String;
}

/// Button that can be used to trigger non-collaborative close if the maker is offline and collaborative close if the
/// maker is online.
export default function CloseButton({ cfd, request, status, buttonTitle, isForceCloseButton }: Props) {
    const disableCloseButton = cfd.state.getGroup() === StateGroupKey.CLOSED
        || ![StateKey.OPEN, StateKey.PENDING_OPEN].includes(cfd.state.key);

    let popoverBody = (
        <>
            <Text>
                This will close your position with the counterparty. The current exchange rate will determine your
                profit/losses.
            </Text>
        </>
    );
    if (isForceCloseButton) {
        popoverBody = (
            <>
                <Text>
                    This will force close your position with the counterparty. The exchange rate at
                </Text>
                {/*Close button is only available if we have a DLC*/}
                <Timestamp timestamp={cfd.expiry_timestamp!} />
                <Text>
                    will determine your profit/losses. It is likely that the rate will change until then.
                </Text>
            </>
        );
    }

    return (
        <Box>
            <Popover
                placement="bottom"
                closeOnBlur={true}
            >
                {({ onClose }) => (
                    <>
                        <PopoverTrigger>
                            <Button colorScheme={"blue"} disabled={disableCloseButton}>{buttonTitle}</Button>
                        </PopoverTrigger>
                        <PopoverContent color="white" bg="blue.800" borderColor="blue.800">
                            <PopoverHeader pt={4} fontWeight="bold" border="0">
                                {buttonTitle} your position
                            </PopoverHeader>
                            <PopoverArrow />
                            <PopoverCloseButton />
                            <PopoverBody>
                                {popoverBody}
                            </PopoverBody>
                            <PopoverFooter
                                border="0"
                                display="flex"
                                alignItems="center"
                                justifyContent="space-between"
                                pb={4}
                            >
                                <Button
                                    size="sm"
                                    colorScheme="red"
                                    onClick={() => {
                                        console.log(`Closing CFD ${cfd.order_id}`);
                                        request([{}]);
                                        onClose();
                                    }}
                                    isLoading={status}
                                >
                                    {buttonTitle} Position
                                </Button>
                            </PopoverFooter>
                        </PopoverContent>
                    </>
                )}
            </Popover>
        </Box>
    );
}
