import { Alert, AlertDescription, AlertIcon, AlertStatus, AlertTitle } from "@chakra-ui/react";
import * as React from "react";

interface AlertBoxProps {
    title: string;
    description: string;
    status?: AlertStatus;
}

export default function AlertBox({ title, description, status = "error" }: AlertBoxProps) {
    return (<Alert status={status}>
        <AlertIcon />
        <AlertTitle mr={2}>{title}</AlertTitle>
        <AlertDescription>{description}</AlertDescription>
    </Alert>);
}
