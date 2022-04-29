import { Alert, AlertDescription, AlertIcon, AlertStatus, AlertTitle, Link } from "@chakra-ui/react";
import * as React from "react";
import { Link as ReachLink } from "react-router-dom";

interface AlertBoxProps {
    title: string;
    description: string;
    status?: AlertStatus;
    reachLinkTo?: string;
}

export default function AlertBox({ title, description, status = "error", reachLinkTo }: AlertBoxProps) {
    return (
        <Alert status={status}>
            <AlertIcon />
            <AlertTitle mr={2}>{title}</AlertTitle>
            {reachLinkTo
                ? (
                    <Link as={ReachLink} to={reachLinkTo}>
                        <AlertDescription textDecor={"underline"}>
                            {description}
                        </AlertDescription>
                    </Link>
                )
                : (
                    <AlertDescription>
                        {description}
                    </AlertDescription>
                )}
        </Alert>
    );
}
