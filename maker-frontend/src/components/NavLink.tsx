import { Button } from "@chakra-ui/react";
import React from "react";

import { Link as RouteLink, useMatch } from "react-router-dom";

type NavLinkProps = { text: string; path: string };

export default function NavLink({ text, path }: NavLinkProps) {
    const match = useMatch(path);
    return (
        <RouteLink to={path}>
            <Button width="100px" colorScheme="blue" variant={match?.path ? "solid" : "outline"}>
                {text}
            </Button>
        </RouteLink>
    );
}
