import { Text } from "@chakra-ui/react";
import React from "react";
import { unixTimestampToDate } from "./Types";

interface Props {
    timestamp: number;
}

export default function Timestamp(
    {
        timestamp,
    }: Props,
) {
    return (
        <Text>
            {unixTimestampToDate(timestamp).toLocaleDateString("en-US", {
                year: "numeric",
                month: "numeric",
                day: "numeric",
                hour: "2-digit",
                minute: "2-digit",
                second: "2-digit",
            })}
        </Text>
    );
}
