import { Box, Center, HStack, Text, useColorModeValue } from "@chakra-ui/react";
import * as React from "react";
import { SocialLinks } from "./SocialLinks";

export default function Footer() {
    return (
        <Box
            bg={useColorModeValue("gray.100", "gray.900")}
            color={useColorModeValue("gray.700", "gray.200")}
        >
            <Center>
                <HStack h={16} alignItems={"center"}>
                    <Text fontSize={"20"} fontWeight={"bold"}>Contact us:</Text>
                    <SocialLinks />
                </HStack>
            </Center>
        </Box>
    );
}
