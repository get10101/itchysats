import { Alert, AlertDescription, AlertIcon, AlertTitle } from "@chakra-ui/react";
import * as React from "react";

interface AlertBoxProps {
    title: string;
    description: string;
}

export default function AlertBox({ title, description }: AlertBoxProps) {
    return (<Alert status="error">
        <AlertIcon />
        <AlertTitle mr={2}>{title}</AlertTitle>
        <AlertDescription>{description}</AlertDescription>
    </Alert>);
}
