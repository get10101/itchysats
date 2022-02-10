import { Heading, VStack } from "@chakra-ui/react";
import React from "react";

export interface TakerId {
    id: string;
}

interface Props {
    takers: TakerId[];
}

const ConnectedTakers = ({ takers }: Props) => {
    return (
        <VStack spacing={3}>
            <Heading size={"sm"} padding={2}>{"Connected takers: " + takers.length}</Heading>
        </VStack>
    );
};

export default ConnectedTakers;
