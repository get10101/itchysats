import { ExternalLinkIcon } from "@chakra-ui/icons";

import { Alert, Center, HStack, Link } from "@chakra-ui/react";

import React from "react";

export default function PromoBanner() {
    return (<HStack>
        <Center>
            <Alert status="info">
                <Link href="http://testing.itchysats.network/" isExternal>
                    🎁Celebrating pitching at Bitcoin2022 in Miami... 🎉
                    <ExternalLinkIcon mx="2px" />
                </Link>
            </Alert>
        </Center>
    </HStack>);
}
