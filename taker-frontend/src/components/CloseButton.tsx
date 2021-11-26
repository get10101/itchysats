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

interface Props {
    cfd: Cfd;
    request: (req: any) => void;
    status: boolean;
    action: String;
}

/// Button that can be used to trigger non-collaborative close if the maker is offline and collaborative close if the
/// maker is online.
export default function CloseButton({ cfd, request, status, action }: Props) {
    const disableCloseButton = cfd.state.getGroup() === StateGroupKey.CLOSED
        || !(cfd.state.key === StateKey.OPEN);

    return <Box w={"45%"}>
        <Popover
            placement="bottom"
            closeOnBlur={true}
        >
            {({ onClose }) => (<>
                <PopoverTrigger>
                    <Button colorScheme={"blue"} disabled={disableCloseButton}>{action}</Button>
                </PopoverTrigger>
                <PopoverContent color="white" bg="blue.800" borderColor="blue.800">
                    <PopoverHeader pt={4} fontWeight="bold" border="0">
                        {action} your position
                    </PopoverHeader>
                    <PopoverArrow />
                    <PopoverCloseButton />
                    <PopoverBody>
                        <Text>
                            This will {action} your position with the counterparty. The exchange rate at {cfd
                                .expiry_timestamp}
                            will determine your profit/losses. It is likely that the rate will change until then.
                        </Text>
                    </PopoverBody>
                    <PopoverFooter
                        border="0"
                        d="flex"
                        alignItems="center"
                        justifyContent="space-between"
                        pb={4}
                    >
                        <Button
                            size="sm"
                            colorScheme="red"
                            onClick={() => {
                                console.log(`Closing CFD ${cfd.order_id}`);
                                request({});
                                onClose();
                            }}
                            isLoading={status}
                        >
                            {action}
                        </Button>
                    </PopoverFooter>
                </PopoverContent>
            </>)}
        </Popover>
    </Box>;
}
